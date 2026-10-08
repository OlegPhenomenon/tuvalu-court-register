#!/usr/bin/env python3
"""HTTP half of scripts/clean-install-check.sh (Python 3 standard library only).

Subcommands (all print `PASS|FAIL <step>: <detail>` lines and exit 1 on any FAIL):
  init   <state.json>                 generate DEMO accounts + temporary passwords; print TSV for the shell
  phase1 <state.json> <base-url>      first login (password -> TOTP enrol -> forced change), reference data,
                                      real case data, snapshot through the API
  phase2 <state.json> <base-url>      fresh logins on the restored instance, snapshot, compare with phase1
"""
import base64
import datetime
import hashlib
import hmac
import json
import secrets
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

FAILURES = []


def report(ok, step, detail=""):
    print(f"{'PASS' if ok else 'FAIL'} {step}{': ' + detail if detail else ''}", flush=True)
    if not ok:
        FAILURES.append(step)
    return ok


def need(ok, step, detail=""):
    """A failed prerequisite makes the following steps meaningless: stop the phase."""
    if not report(ok, step, detail):
        raise SystemExit(1)


# ---------------------------------------------------------------- accounts

USERS = {
    # key: (username, display name, create-user flags, permissions granted afterwards via `grant`)
    "admin": ("demo.admin", "DEMO Technical Administrator", ["--perm", "admin.users", "--perm", "admin.settings"], []),
    "clerk": ("demo.clerk", "DEMO Registry Clerk",
              ["--perm", "intake.manage", "--perm", "case.register", "--perm", "case.edit", "--perm", "document.manage"],
              ["hearing.schedule", "task.manage"]),
    "head": ("demo.head", "DEMO Head of Registry",
             ["--perm", "case.view_all", "--perm", "case.assign_staff", "--perm", "case.assign_judge"],
             ["document.grant_restricted", "audit.view"]),
    "judge": ("demo.judge", "DEMO Judge", ["--judge", "--perm", "decision.draft", "--perm", "decision.finalise"],
              ["document.manage"]),
    "officer": ("demo.officer", "DEMO Service Officer", ["--perm", "dispatch.manage"], ["hearing.schedule"]),
}


def cmd_init(path):
    state = {"users": {}}
    for key, (username, display, flags, grants) in USERS.items():
        state["users"][key] = {
            "username": username,
            "display": display,
            "temp_password": "DEMO-temp-" + secrets.token_urlsafe(12),
            "new_password": "DEMO-final-" + secrets.token_urlsafe(12),
            "flags": flags,
            "grants": grants,
        }
    with open(path, "w") as f:
        json.dump(state, f, indent=1)
    for u in state["users"].values():
        # username, display, temp password, flags, CLI grants (tab separated; spaces inside fields are fine)
        print("\t".join([u["username"], u["display"], u["temp_password"], " ".join(u["flags"]), " ".join(u["grants"])]))


# ---------------------------------------------------------------- TOTP (RFC 6238, SHA1, 6 digits, 30 s)

def totp_at(secret_b32, step):
    key = base64.b32decode(secret_b32 + "=" * (-len(secret_b32) % 8))
    h = hmac.new(key, struct.pack(">Q", step), hashlib.sha1).digest()
    o = h[-1] & 0x0F
    return "%06d" % ((struct.unpack(">I", h[o:o + 4])[0] & 0x7FFFFFFF) % 1_000_000)


def fresh_code(user):
    """A code the server accepts once: its step must exceed the last step used (replay protection).
    The server accepts the current step and one either side; wait for a new step when needed."""
    last = user.get("last_step", -1)
    while True:
        now = int(time.time()) // 30
        for step in (now, now + 1):
            if step > last:
                user["last_step"] = step
                return totp_at(user["secret"], step)
        time.sleep(30 - time.time() % 30 + 0.5)


# ---------------------------------------------------------------- HTTP client

class Client:
    def __init__(self, base, cookie=None):
        self.base = base.rstrip("/")
        self.cookie = cookie

    def request(self, method, path, body=None, multipart=None, raw=False):
        headers = {}
        data = None
        if method != "GET":
            headers["X-TCR"] = "1"
        if multipart is not None:
            fields, filename, blob = multipart
            boundary = "----tcrcleaninstall" + secrets.token_hex(8)
            parts = b""
            for k, v in fields.items():
                parts += f'--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode()
            parts += (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{filename}"\r\n'
                      "Content-Type: application/pdf\r\n\r\n").encode() + blob + f"\r\n--{boundary}--\r\n".encode()
            data = parts
            headers["Content-Type"] = f"multipart/form-data; boundary={boundary}"
        elif body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        if self.cookie:
            headers["Cookie"] = f"tcr_session={self.cookie}"
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=60) as res:
                status, hdrs, payload = res.status, res.headers, res.read()
        except urllib.error.HTTPError as e:
            status, hdrs, payload = e.code, e.headers, e.read()
        for value in hdrs.get_all("Set-Cookie") or []:
            if value.startswith("tcr_session="):
                self.cookie = value.split(";", 1)[0].split("=", 1)[1] or None
        if raw:
            return status, payload
        try:
            return status, json.loads(payload) if payload else None
        except ValueError:
            return status, payload.decode(errors="replace")

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, body=None):
        return self.request("POST", path, body if body is not None else {})


def err_code(body):
    return body.get("error", {}).get("code") if isinstance(body, dict) else None


def ok_json(step, res, detail=""):
    status, body = res
    need(200 <= status < 300, step, f"HTTP {status} {detail}".strip() if 200 <= status < 300 else f"HTTP {status}: {body}")
    return body


# ---------------------------------------------------------------- fixtures

def pdf(title, lines):
    """Small valid single-page PDF with a correct xref table (same structure as the product's demo PDFs)."""
    def esc(s):
        return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    content = "BT /F1 18 Tf 72 770 Td (DEMO - FICTIONAL - NOT A COURT RECORD) Tj ET\n"
    content += f"BT /F1 14 Tf 72 730 Td ({esc(title)}) Tj ET\n"
    y = 700
    for line in lines:
        content += f"BT /F1 11 Tf 72 {y} Td ({esc(line)}) Tj ET\n"
        y -= 16
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        f"<< /Length {len(content)} >>\nstream\n{content}endstream",
    ]
    out = b"%PDF-1.4\n"
    offsets = []
    for i, obj in enumerate(objects):
        offsets.append(len(out))
        out += f"{i + 1} 0 obj\n{obj}\nendobj\n".encode()
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for o in offsets:
        out += f"{o:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return out


COURT_TZ = datetime.timezone(datetime.timedelta(hours=12))  # Pacific/Funafuti, no DST


def court_today():
    return datetime.datetime.now(COURT_TZ).date()


def next_weekday(days_ahead):
    d = court_today() + datetime.timedelta(days=days_ahead)
    while d.weekday() >= 5:
        d += datetime.timedelta(days=1)
    return d


# ---------------------------------------------------------------- phase 1

def first_login(base, key, user):
    name = user["username"]
    c = Client(base)
    status, body = c.post("/api/auth/login", {"username": name, "password": user["temp_password"]})
    need(status == 200 and body.get("enroll_required") is True, f"[{key}] password login with temporary password",
         f"HTTP {status} {body}")
    status, body = c.get("/api/cases")
    report(status == 401, f"[{key}] records blocked before TOTP enrolment", f"GET /api/cases -> HTTP {status} {err_code(body)}")
    body = ok_json(f"[{key}] TOTP setup", c.post("/api/auth/totp/setup"))
    uri = urllib.parse.urlparse(body["otpauth_uri"])
    query = urllib.parse.parse_qs(uri.query)
    need(uri.scheme == "otpauth" and uri.netloc == "totp" and query.get("digits") == ["6"] and query.get("period") == ["30"],
         f"[{key}] otpauth URI well formed", body["otpauth_uri"].split("?")[0] + "?secret=<redacted>&...")
    user["secret"] = query["secret"][0]
    status, body = c.post("/api/auth/totp/enable", {"code": fresh_code(user)})
    need(status == 200 and body.get("must_change_password") is True and body.get("mfa_enrolled") is True,
         f"[{key}] TOTP enrolment with computed code", f"HTTP {status} must_change_password={body.get('must_change_password')}")
    status, body = c.get("/api/cases")
    report(status == 403 and err_code(body) == "password_change_required",
           f"[{key}] records endpoint 403 password_change_required before change", f"HTTP {status} {err_code(body)}")
    status, body = c.post("/api/auth/password", {"current": user["temp_password"], "new": user["new_password"]})
    report(status == 401 and err_code(body) == "bad_code", f"[{key}] password change without fresh TOTP refused",
           f"HTTP {status} {err_code(body)}")
    status, body = c.post("/api/auth/password",
                          {"current": user["temp_password"], "new": user["new_password"], "code": fresh_code(user)})
    need(status == 200, f"[{key}] forced password change with fresh TOTP", f"HTTP {status} {body}")
    me = ok_json(f"[{key}] /api/auth/me after change", c.get("/api/auth/me"))
    report(me["must_change_password"] is False and me["mode"] == "production",
           f"[{key}] must_change_password cleared", f"mode={me['mode']} perms={sorted(me['user']['perms'])}")
    user["id"] = me["user"]["id"]
    user["perms_me"] = sorted(me["user"]["perms"])
    flags = user["flags"]
    expected = sorted(set([flags[i + 1] for i, f in enumerate(flags) if f == "--perm"] + user["grants"]))
    report(user["perms_me"] == expected and me["user"]["is_judge"] == ("--judge" in flags),
           f"[{key}] permissions = create-user --perm + CLI grants (revoked grant absent)",
           f"judge={me['user']['is_judge']} expected={expected}")
    status, body = c.get("/api/cases")
    if key == "admin":
        report(status == 200 and body.get("items") == [], f"[{key}] records endpoint after change (tech admin sees no cases)",
               f"HTTP {status} items={body.get('items') if isinstance(body, dict) else body}")
    else:
        report(status == 200, f"[{key}] records endpoint works after change", f"HTTP {status}")
    user["cookie_instance1"] = c.cookie
    return c


def phase1(path, base):
    state = json.load(open(path))
    U = state["users"]
    s, body = Client(base).get("/api/auth/mode")
    need(s == 200 and body["mode"] == "production", "instance reports production mode", str(body))
    cl = {k: first_login(base, k, U[k]) for k in U}

    # Reference data a fresh production installation needs (Settings, admin.settings).
    admin = cl["admin"]
    unit = ok_json("[admin] create court unit", admin.post("/api/admin/court-units",
                                                         {"code": "DEMO-MC", "name": "DEMO Magistrates Court (fictional)"}))
    unit_id = unit.get("id")
    reg = ok_json("[admin] create registry DEMO-CIV", admin.post("/api/admin/registries",
                  {"court_unit_id": unit_id, "series": "DEMO-CIV", "name": "DEMO civil register"}))
    room = ok_json("[admin] create room", admin.post("/api/admin/rooms",
                   {"name": "DEMO Courtroom 1", "location": "Ground floor", "court_unit_id": unit_id}))
    settings = ok_json("[admin] read settings", admin.get("/api/admin/settings"))
    state["installation_id_api"] = settings.get("installation_id")
    report(bool(state["installation_id_api"]), "[admin] installation id visible in Settings", str(state["installation_id_api"]))
    report("no antivirus" in str(settings.get("file_scanner")) and "DEMO" not in str(settings.get("file_scanner")),
           "[admin] Settings states the TCR_AV=off choice", repr(settings.get("file_scanner")))

    # Intake -> registration.
    clerk = cl["clerk"]
    intake = ok_json("[clerk] create intake", clerk.post("/api/intakes", {
        "sender_name": "Alexei Fenwick (DEMO)", "channel": "counter", "origin_island": "funafuti",
        "received_date": court_today().isoformat(), "description": "DEMO claim about an unpaid boat repair",
        "is_paper_original": True, "paper_location": "DEMO registry cabinet A, folder 12"}))
    ok_json("[clerk] mark intake ready", clerk.post(f"/api/intakes/{intake['id']}/mark-ready"))
    refs = ok_json("[clerk] reference lists", clerk.get("/api/ref"))
    registry_id = next(r["id"] for r in refs["registries"] if r["series"] == "DEMO-CIV")
    reg_res = ok_json("[clerk] register case", clerk.post(f"/api/intakes/{intake['id']}/register", {
        "registry_id": registry_id, "category": "civil_contract", "title": "DEMO Fenwick v Calder (clean install)",
        "participants": [
            {"new_party": {"kind": "person", "name": "Alexei Fenwick (DEMO)", "contact_email": "alexei@example.invalid"},
             "role": "claimant", "service_contact": "alexei@example.invalid"},
            {"new_party": {"kind": "person", "name": "Maria Calder (DEMO)", "contact_email": "maria@example.invalid"},
             "role": "respondent", "service_contact": "maria@example.invalid"}]}))
    cid = reg_res["case_id"]
    state["case_id"], state["case_number"] = cid, reg_res["number"]
    report(reg_res["number"].startswith("DEMO-CIV"), "[clerk] case number issued from DEMO-CIV", reg_res["number"])

    # Document v1 + v2.
    v1 = pdf("DEMO statement of claim", ["Version 1", "Fictional clean-install check"])
    doc = ok_json("[clerk] upload PDF (version 1)", clerk.request("POST", f"/api/cases/{cid}/documents", multipart=(
        {"title": "DEMO statement of claim", "doc_type": "other", "source": "court", "visibility": "administrative"},
        "claim-v1.pdf", v1)))
    doc_id = doc["id"]
    v2 = pdf("DEMO statement of claim", ["Version 2 (corrected)", "Fictional clean-install check"])
    doc2 = ok_json("[clerk] upload second version", clerk.request("POST", f"/api/documents/{doc_id}/versions", multipart=(
        {"note": "DEMO corrected claim"}, "claim-v2.pdf", v2)))
    versions = doc2.get("versions", [])
    report(len(versions) == 2 and all(v.get("scan_status") == "clean" and "no antivirus" in str(v.get("scan_note"))
                                      for v in versions),
           "[clerk] document has 2 clean versions labelled format checks only",
           ", ".join(f"v{v.get('version_no')} {v.get('scan_status')} '{v.get('scan_note')}'" for v in versions))
    expected = {hashlib.sha256(v1).hexdigest(), hashlib.sha256(v2).hexdigest()}
    for v in versions:
        st, blob = clerk.request("GET", f"/api/document-versions/{v['id']}/download", raw=True)
        report(st == 200 and hashlib.sha256(blob).hexdigest() in expected,
               f"[clerk] download v{v.get('version_no')} matches uploaded bytes", hashlib.sha256(blob).hexdigest())
    restricted = ok_json("[clerk] upload restricted document", clerk.request("POST", f"/api/cases/{cid}/documents", multipart=(
        {"title": "DEMO medical report (restricted)", "doc_type": "evidence", "source": "party", "visibility": "restricted"},
        "restricted.pdf", pdf("DEMO medical report", ["Restricted fictional material"]))))
    state["restricted_doc_id"] = restricted["id"]

    # Assignments (registry head).
    head = cl["head"]
    ok_json("[head] assign judge", head.post(f"/api/cases/{cid}/assignments",
            {"user_id": U["judge"]["id"], "role": "judge", "reason": "DEMO allocation by list"}))
    ok_json("[head] assign service officer", head.post(f"/api/cases/{cid}/assignments",
            {"user_id": U["officer"]["id"], "role": "service_officer", "reason": "DEMO service of notices"}))
    ok_json("[head] grant restricted document to judge", head.post(f"/api/documents/{restricted['id']}/grants",
            {"user_id": U["judge"]["id"], "reason": "DEMO judge needs the report"}))

    # Hearing (clerk; judge defaults to the assigned judge).
    day = next_weekday(14).isoformat()
    hearing = ok_json("[clerk] schedule hearing", clerk.post(f"/api/cases/{cid}/hearings", {
        "hearing_type": "hearing", "starts_local": f"{day}T09:00", "ends_local": f"{day}T10:00",
        "room_id": room["id"], "confirm": True}))
    report(hearing.get("status") == "scheduled" and hearing.get("judge_name") == U["judge"]["display"],
           "[clerk] hearing scheduled with assigned judge", f"{hearing.get('starts_local')} status={hearing.get('status')} judge={hearing.get('judge_name')}")

    # Draft decision (judge).
    judge = cl["judge"]
    ddoc = ok_json("[judge] upload draft ruling PDF", judge.request("POST", f"/api/cases/{cid}/documents", multipart=(
        {"title": "DEMO draft ruling", "doc_type": "decision", "source": "court", "visibility": "administrative"},
        "ruling.pdf", pdf("DEMO draft ruling", ["Fictional draft decision"]))))
    dvid = ddoc["versions"][0]["id"]
    decision = ok_json("[judge] draft decision", judge.post(f"/api/cases/{cid}/decisions",
                       {"title": "DEMO decision on the claim", "document_version_id": dvid, "hearing_id": hearing["id"]}))
    report(decision.get("status") == "draft", "[judge] decision is a draft", f"id={decision.get('id')}")

    s, body = admin.get(f"/api/cases/{cid}")
    s2, body2 = admin.get("/api/cases")
    report(s == 404 and s2 == 200 and body2.get("items") == [], "[admin] technical administrator cannot see the case",
           f"GET /api/cases/{cid} -> HTTP {s}; list items={len(body2.get('items', []))}")

    state["snapshot"] = snapshot(cl, state)
    n_versions = sum(len(d.get("versions", [])) for d in state["snapshot"]["documents"].values())
    report("versions" in state["snapshot"]["documents"].get(f"judge:{restricted['id']}", {}),
           "[judge] restricted document visible through explicit grant", f"{len(state['snapshot']['documents'])} viewer/document entries, {n_versions} versions")
    for key, user in U.items():
        st, _ = Client(base, user["cookie_instance1"]).get("/api/auth/me")
        report(st == 200, f"[{key}] session cookie live on instance 1 just before backup", f"GET /api/auth/me -> HTTP {st}")
    json.dump(state, open(path, "w"), indent=1)


# ---------------------------------------------------------------- snapshot / compare

def snapshot(cl, state):
    cid = state["case_id"]
    head, judge, admin = cl["head"], cl["judge"], cl["admin"]
    snap = {}
    cases = ok_json("snapshot cases", head.get("/api/cases"))
    snap["cases"] = cases
    detail = ok_json("snapshot case detail", head.get(f"/api/cases/{cid}"))
    snap["case_detail"] = detail
    snap["assignments"] = detail.get("assignments")
    docs = {}
    for who, c in (("head", head), ("judge", judge)):
        ids = []
        for url in (f"/api/cases/{cid}/documents", f"/api/cases/{cid}/restricted-documents"):
            st, listing = c.get(url)
            snap.setdefault("listing_status", {})[f"{who} {url}"] = st
            if st == 200:
                ids += [d["id"] for d in listing.get("items", [])]
        for doc_id in sorted(set(ids)):
            st, full = c.get(f"/api/documents/{doc_id}")
            if st != 200:  # e.g. restricted metadata listed for granting, content not viewable by this person
                docs[f"{who}:{doc_id}"] = {"detail_status": st}
                continue
            for v in full.get("versions", []):
                st, blob = c.request("GET", f"/api/document-versions/{v['id']}/download", raw=True)
                need(st == 200, f"snapshot download version {v['id']} ({who})", f"HTTP {st}")
                v["downloaded_sha256"] = hashlib.sha256(blob).hexdigest()
                v["downloaded_size"] = len(blob)
                if v.get("sha256"):
                    need(v["sha256"] == v["downloaded_sha256"], f"snapshot version {v['id']} stored SHA-256 matches download")
            docs[f"{who}:{doc_id}"] = full
    snap["documents"] = docs
    snap["hearings"] = ok_json("snapshot hearings", head.get(f"/api/cases/{cid}/hearings"))
    snap["decisions"] = ok_json("snapshot decisions", judge.get(f"/api/cases/{cid}/decisions"))
    users = ok_json("snapshot users and permissions", admin.get("/api/admin/users"))
    snap["users"] = users
    snap["installation_id"] = ok_json("snapshot settings", admin.get("/api/admin/settings")).get("installation_id")
    return snap


def strip_volatile(v):
    """Remove fields that legitimately differ between two running instances (session/login bookkeeping)."""
    volatile = {"last_login_at", "last_seen_at", "active_sessions", "sessions", "locked_until", "failed_logins"}
    if isinstance(v, dict):
        return {k: strip_volatile(x) for k, x in v.items() if k not in volatile}
    if isinstance(v, list):
        return [strip_volatile(x) for x in v]
    return v


def diff(a, b, path=""):
    if type(a) is not type(b):
        return [f"{path}: {json.dumps(a)[:120]} != {json.dumps(b)[:120]}"]
    if isinstance(a, dict):
        out = []
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                out.append(f"{path}.{k}: {'missing before' if k not in a else 'missing after'}")
            else:
                out += diff(a[k], b[k], f"{path}.{k}")
        return out
    if isinstance(a, list):
        if len(a) != len(b):
            return [f"{path}: length {len(a)} != {len(b)}"]
        return [x for i in range(len(a)) for x in diff(a[i], b[i], f"{path}[{i}]")]
    return [] if a == b else [f"{path}: {json.dumps(a)[:120]} != {json.dumps(b)[:120]}"]


def phase2(path, base):
    state = json.load(open(path))
    U = state["users"]
    s, body = Client(base).get("/api/auth/mode")
    need(s == 200 and body["mode"] == "production", "restored instance reports production mode", str(body))

    # Restore revokes every session in the snapshot (docs/OPERATIONS.md): the cookies that were live on
    # instance 1 when the backup was taken (they were used for its final snapshot) must now get 401.
    for key, user in U.items():
        old = Client(base, user["cookie_instance1"])
        s_me, b_me = old.get("/api/auth/me")
        s_cases, b_cases = old.get("/api/cases")
        report(s_me == 401 and s_cases == 401, f"[{key}] instance-1 session cookie rejected on restored instance",
               f"GET /api/auth/me -> HTTP {s_me} {err_code(b_me)}; GET /api/cases -> HTTP {s_cases} {err_code(b_cases)}")

    cl = {}
    for key, user in U.items():
        c = Client(base)  # no cookie: a fresh browser
        s, body = c.post("/api/auth/login", {"username": user["username"], "password": user["temp_password"]})
        report(s == 401 and err_code(body) == "bad_credentials", f"[{key}] temporary password rejected after restore",
               f"HTTP {s} {err_code(body)}")
        s, body = c.post("/api/auth/login", {"username": user["username"], "password": user["new_password"]})
        need(s == 200 and body.get("mfa_required") is True and body.get("enroll_required") is False,
             f"[{key}] login with changed password on restored instance", f"HTTP {s} {body}")
        s, body = c.post("/api/auth/totp", {"code": fresh_code(user)})
        need(s == 200 and body.get("must_change_password") is False,
             f"[{key}] TOTP sign-in with restored secret", f"HTTP {s} must_change_password={body.get('must_change_password') if isinstance(body, dict) else body}")
        report(sorted(body["user"]["perms"]) == user["perms_me"] and body["user"]["id"] == user["id"],
               f"[{key}] same user id and permissions after restore", f"id={body['user']['id']} perms={sorted(body['user']['perms'])}")
        cl[key] = c

    snap = snapshot(cl, state)
    before = strip_volatile(state["snapshot"])
    after = strip_volatile(snap)
    labels = {
        "cases": "cases list identical",
        "case_detail": "case detail identical (participants, status, history)",
        "assignments": "assignments identical",
        "documents": "documents, versions, download SHA-256 and grants identical",
        "hearings": "hearings identical",
        "decisions": "decisions identical",
        "users": "users and permissions identical",
        "installation_id": "installation id identical",
        "listing_status": "document listing access per viewer identical",
    }
    for k, label in labels.items():
        d = diff(before.get(k), after.get(k), k)
        report(not d, f"compare: {label}", "; ".join(d[:8]) if d else "")
    shas = sorted({v["downloaded_sha256"] for doc in snap["documents"].values() for v in doc.get("versions", [])})
    print(f"INFO version SHA-256 after restore: {', '.join(s[:16] + '…' for s in shas)} ({len(shas)} files)", flush=True)
    report(snap["installation_id"] == state["installation_id_api"], "installation id equals instance 1 Settings value",
           str(snap["installation_id"]))

    # The restored instance is writable (files directory and database in the new volume).
    doc_id = next(int(k.split(":")[1]) for k, d in snap["documents"].items() if len(d.get("versions", [])) == 2)
    s, body = cl["clerk"].request("POST", f"/api/documents/{doc_id}/versions", multipart=(
        {"note": "DEMO post-restore version"}, "claim-v3.pdf", pdf("DEMO statement of claim", ["Version 3 after restore"])))
    report(s == 200 and len(body.get("versions", [])) == 3, "restored instance accepts a new document version",
           f"HTTP {s}")
    json.dump(state, open(path, "w"), indent=1)


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 64
    cmd, path = sys.argv[1], sys.argv[2]
    try:
        if cmd == "init":
            cmd_init(path)
        elif cmd == "phase1":
            phase1(path, sys.argv[3])
        elif cmd == "phase2":
            phase2(path, sys.argv[3])
        else:
            print(__doc__, file=sys.stderr)
            return 64
    except SystemExit:
        pass
    except Exception as e:  # unexpected response shape etc.: a failure, never a silent pass
        report(False, f"{cmd} aborted", f"{type(e).__name__}: {e}")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
