#!/usr/bin/env bash
# Clean-install + backup/restore verification of the Tuvalu Court Register in isolated Docker
# containers. Builds the image from the repo Dockerfile, runs a PRODUCTION container on a fresh
# named volume, creates accounts only through the documented maintenance commands, drives the
# HTTP API as a real client (scripts/clean_install_check.py), takes an encrypted backup, restores
# it into a NEW empty volume, starts a second container and compares both installations.
#
# Usage: scripts/clean-install-check.sh            (from any directory; needs docker + python3)
# Exit status: 0 when every step passes, 1 otherwise. Containers, volumes, the image and the
# temporary directory are always removed (EXIT trap). All data created is fictional (DEMO).
#
# Docker translation of docs/OPERATIONS.md "Docker Compose": `docker compose exec [-i] court X` is
# run as `docker exec [-i] <container> X` (scripted stdin: -i without -t), `docker compose cp` as
# `docker cp`, `docker compose run --rm court …` as `docker run --rm <same env + volume> <image> …`.
# The backup key follows the documented custody: exported to a (simulated, local private) key vault
# and deleted from the volume; retrieved only for restore and deleted again. The containers get the
# same runtime limits as docker-compose.yml (read-only root, /tmp tmpfs, 256 MB, 0.5 CPU, loopback).
set -u

REPO=$(cd "$(dirname "$0")/.." && pwd)
HELPER="$REPO/scripts/clean_install_check.py"
RUN_ID="tcrci-$(date +%Y%m%d%H%M%S)-$$"
IMAGE="tuvalu-court-cleaninstall:$RUN_ID"
VOL_UNINIT="$RUN_ID-uninit"
VOL_NOAV="$RUN_ID-noav"
VOL1="$RUN_ID-data"
VOL2="$RUN_ID-restored"
C_NOAV="$RUN_ID-noav"
C1="$RUN_ID-court"
C2="$RUN_ID-court-restored"
WORK=$(mktemp -d)
BIN=/usr/local/bin/tuvalu-court
# Production environment used for every container; the reasons are printed in the start step.
PROD_ENV=(-e TCR_MODE=production -e TCR_AV=off -e TCR_COOKIE_SECURE=false)
# Same limits as docker-compose.yml; --health-interval only shortens the Dockerfile HEALTHCHECK period.
LIMITS=(--read-only --tmpfs /tmp --memory 256m --cpus 0.5 --health-interval 3s)

FAILS=0
STEP=0
SUMMARY=()

log() { printf '%s\n' "$*"; }
step() { STEP=$((STEP + 1)); log ""; log "=== Step $STEP: $* [$(date -u +%H:%M:%SZ)]"; }
pass() { log "PASS $*"; SUMMARY+=("PASS $*"); }
fail() { log "FAIL $*"; SUMMARY+=("FAIL $*"); FAILS=$((FAILS + 1)); }
check() { local d=$1; shift; if "$@"; then pass "$d"; else fail "$d"; fi; }
abort() { fail "$*"; log "ABORT: later steps depend on this step."; exit 1; }
# Show a command before running it; the output is captured into $OUT (stdout+stderr) and $RC.
run() { log "+ $*"; OUT=$("$@" 2>&1); RC=$?; [ -n "$OUT" ] && printf '%s\n' "$OUT" | sed 's/^/    /'; return 0; }
# Same, feeding a secret on stdin from a private file (docs: `docker compose exec -i … < /secure/temporary-password`).
run_stdin() {
  local secret=$1; shift
  (umask 077; printf '%s\n' "$secret" >"$WORK/temporary-password")
  log "+ $* < temporary-password   # private file; the password is never an argument or logged"
  OUT=$("$@" <"$WORK/temporary-password" 2>&1); RC=$?; rm -f "$WORK/temporary-password"
  [ -n "$OUT" ] && printf '%s\n' "$OUT" | sed 's/^/    /'; return 0
}
contains() { case "$OUT" in *"$1"*) return 0 ;; *) return 1 ;; esac; }
volume_empty() { [ -z "$(docker run --rm -v "$1":/data --entrypoint ls "$IMAGE" -A /data 2>&1)" ]; }
volume_listing() { docker run --rm -v "$1":/data --entrypoint ls "$IMAGE" -A /data 2>&1 | tr '\n' ' '; }
# Random free loopback port, never 8096-8097 (used by a concurrent local demo server).
free_port() {
  local p
  while :; do
    p=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
    [ "$p" != 8096 ] && [ "$p" != 8097 ] && { echo "$p"; return; }
  done
}
wait_health() {
  local port=$1 container=$2 i
  for i in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:$port/api/health" >/dev/null 2>&1; then return 0; fi
    [ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" = true ] || return 1
    sleep 1
  done
  return 1
}
stats() {
  local peak
  peak=$(docker exec "$1" cat /sys/fs/cgroup/memory.peak 2>/dev/null)
  printf '%s cgroup memory.peak=%s\n' \
    "$(docker stats --no-stream --format '{{.Name}} mem={{.MemUsage}} ({{.MemPerc}}) cpu={{.CPUPerc}} pids={{.PIDs}}' "$1")" \
    "$([ -n "$peak" ] && echo "$((peak / 1024))KiB" || echo n/a)"
}
health_status() {
  local i s
  for i in $(seq 1 20); do
    s=$(docker inspect -f '{{.State.Health.Status}}' "$1" 2>/dev/null)
    [ "$s" = healthy ] && break
    sleep 1
  done
  log "    Docker HEALTHCHECK status of $1: $s"
  [ "$s" = healthy ]
}

cleanup() {
  local rc=$?
  log ""
  log "=== Cleanup (trap)"
  docker rm -f "$C_NOAV" "$C1" "$C2" >/dev/null 2>&1
  docker volume rm -f "$VOL_UNINIT" "$VOL_NOAV" "$VOL1" "$VOL2" >/dev/null 2>&1
  docker image rm -f "$IMAGE" >/dev/null 2>&1
  rm -rf "$WORK"
  local left
  left=$(docker ps -a --filter "name=$RUN_ID" -q; docker volume ls -q --filter "name=$RUN_ID"; docker image ls -q "$IMAGE")
  if [ -z "$left" ]; then log "PASS cleanup: containers, volumes, image and temp dir removed"; else log "FAIL cleanup: leftovers: $left"; rc=1; fi
  log ""
  log "=== Summary ($RUN_ID)"
  printf '%s\n' "${SUMMARY[@]}"
  [ "$FAILS" -eq 0 ] && [ "$rc" -eq 0 ] && log "RESULT: PASS" || { log "RESULT: FAIL ($FAILS failed steps)"; rc=1; }
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# --------------------------------------------------------------------------------------------
step "Preflight"
log "repo: $REPO (git $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo '?'))"
log "run id: $RUN_ID"
command -v docker >/dev/null && command -v python3 >/dev/null && command -v curl >/dev/null \
  || abort "docker, python3 and curl are required"
log "docker server: $(docker version --format '{{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}')"
log "python: $(python3 --version)"

# --------------------------------------------------------------------------------------------
step "Build image from the repository Dockerfile"
log "+ docker build -t $IMAGE $REPO"
if docker build -t "$IMAGE" "$REPO" >"$WORK/build.log" 2>&1; then
  SIZE=$(docker image inspect -f '{{.Size}}' "$IMAGE")
  pass "docker build ($IMAGE), image size $((SIZE / 1000000)) MB ($SIZE bytes)"
else
  tail -40 "$WORK/build.log"
  abort "docker build"
fi

# --------------------------------------------------------------------------------------------
step "Negative: maintenance commands against an uninitialised volume refuse and create nothing"
docker volume create "$VOL_UNINIT" >/dev/null
run_stdin "DEMO-never-used-password" docker run --rm -i -v "$VOL_UNINIT":/data "${PROD_ENV[@]}" "$IMAGE" create-user demo.nobody "DEMO Nobody"
check "create-user on uninitialised volume refused (exit $RC)" test "$RC" -ne 0 -a -n "$(contains 'No initialised database' && echo y)"
for cmd in "grant demo.nobody case.view_all" "backup /data/x.tcrb /data/x.key" "verify-audit"; do
  # shellcheck disable=SC2086
  run docker run --rm -v "$VOL_UNINIT":/data "${PROD_ENV[@]}" "$IMAGE" $cmd
  check "'${cmd%% *}' on uninitialised volume refused (exit $RC)" test "$RC" -ne 0 -a -n "$(contains 'No initialised database' && echo y)"
done
run docker run --rm -v "$VOL_UNINIT":/data "$IMAGE" verify-audit
check "maintenance command without TCR_MODE=production refused (exit $RC)" test "$RC" -ne 0 -a -n "$(contains 'require TCR_MODE=production' && echo y)"
check "uninitialised volume still empty (listing: '$(volume_listing "$VOL_UNINIT")')" volume_empty "$VOL_UNINIT"

# --------------------------------------------------------------------------------------------
step "Negative: production without a scanner and without TCR_AV=off refuses to start"
docker volume create "$VOL_NOAV" >/dev/null
run docker run -d --name "$C_NOAV" -e TCR_MODE=production -e TCR_COOKIE_SECURE=false "${LIMITS[@]}" -v "$VOL_NOAV":/data "$IMAGE"
for _ in $(seq 1 20); do [ "$(docker inspect -f '{{.State.Running}}' "$C_NOAV")" = false ] && break; sleep 0.5; done
NOAV_EXIT=$(docker inspect -f '{{.State.Running}} {{.State.ExitCode}}' "$C_NOAV")
log "    state: running/exit = $NOAV_EXIT; log: $(docker logs "$C_NOAV" 2>&1 | tail -2 | tr '\n' ' ')"
check "production without TCR_CLAMD/TCR_AV=off exits non-zero" test "${NOAV_EXIT%% *}" = false -a "${NOAV_EXIT##* }" != 0
docker rm -f "$C_NOAV" >/dev/null 2>&1
log "    volume after refused start: '$(volume_listing "$VOL_NOAV")'"

# --------------------------------------------------------------------------------------------
step "Start PRODUCTION container on a fresh named volume"
log "TCR_AV=off reason: this isolated local check has no court-operated clamd service; uploads are"
log "  format-checked only and labelled 'format checks only, no antivirus'. A real court sets TCR_CLAMD."
log "TCR_COOKIE_SECURE=false reason: the check talks plain http to 127.0.0.1 (no TLS proxy)."
docker volume create "$VOL1" >/dev/null
P1=$(free_port)
run docker run -d --name "$C1" "${LIMITS[@]}" "${PROD_ENV[@]}" -p "127.0.0.1:$P1:8088" -v "$VOL1":/data "$IMAGE"
[ "$RC" -eq 0 ] || abort "docker run $C1"
if wait_health "$P1" "$C1"; then
  pass "GET http://127.0.0.1:$P1/api/health ok"
else
  docker logs "$C1" 2>&1 | tail -20
  abort "server did not become healthy"
fi
docker logs "$C1" 2>&1 | sed 's/^/    log: /'
check "server log says production" sh -c "docker logs '$C1' 2>&1 | grep -q '(production)'"
check "image HEALTHCHECK reports healthy" health_status "$C1"

# --------------------------------------------------------------------------------------------
step "Create accounts with the documented maintenance commands (passwords via stdin)"
python3 "$HELPER" init "$WORK/state.json" >"$WORK/users.tsv" || abort "helper init"
INSTALL_ID=""
TAB=$(printf '\t')
while IFS="$TAB" read -r U_NAME U_DISPLAY U_PW U_FLAGS U_GRANTS; do
  # shellcheck disable=SC2086
  run_stdin "$U_PW" docker exec -i "$C1" "$BIN" create-user "$U_NAME" "$U_DISPLAY" $U_FLAGS
  ID=$(printf '%s\n' "$OUT" | sed -n 's/^Installation: //p')
  [ -z "$INSTALL_ID" ] && INSTALL_ID=$ID
  check "create-user $U_NAME (prints data dir /data and installation $ID)" \
    test "$RC" -eq 0 -a -n "$(contains "Created user" && contains "Data directory: /data" && echo y)" -a "$ID" = "$INSTALL_ID" -a -n "$ID"
  for PERM in $U_GRANTS; do
    run docker exec "$C1" "$BIN" grant "$U_NAME" "$PERM"
    check "grant $U_NAME $PERM" test "$RC" -eq 0 -a -n "$(contains 'grant ok' && echo y)"
  done
done <"$WORK/users.tsv"
run docker exec "$C1" "$BIN" grant demo.clerk report.view
check "grant demo.clerk report.view" test "$RC" -eq 0
run docker exec "$C1" "$BIN" revoke demo.clerk report.view
check "revoke demo.clerk report.view (verified absent at login)" test "$RC" -eq 0 -a -n "$(contains 'revoke ok' && echo y)"
run_stdin "short-pw" docker exec -i "$C1" "$BIN" create-user demo.short "DEMO Short"
check "create-user with a password under 12 characters refused" test "$RC" -ne 0
run docker exec "$C1" "$BIN" grant demo.clerk no.such_permission
check "grant of an unknown permission refused" test "$RC" -ne 0
log "installation id (CLI): $INSTALL_ID"

# --------------------------------------------------------------------------------------------
step "HTTP client: first login, TOTP enrolment, forced password change, real case data"
log "+ python3 scripts/clean_install_check.py phase1 <state> http://127.0.0.1:$P1"
python3 "$HELPER" phase1 "$WORK/state.json" "http://127.0.0.1:$P1" | sed 's/^/    /'
PH1=${PIPESTATUS[0]}
check "phase 1 HTTP checks (see indented lines)" test "$PH1" -eq 0
[ "$PH1" -eq 0 ] || abort "phase 1 failed; backup comparison would be meaningless"
API_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["installation_id_api"])' "$WORK/state.json")
check "installation id in Settings ($API_ID) equals CLI output" test "$API_ID" = "$INSTALL_ID"
STATS1=$(stats "$C1")
log "docker stats (instance 1, after phase 1): $STATS1"

# --------------------------------------------------------------------------------------------
step "Backup with a generated key, key exported to a vault and removed from the volume (docs: Docker Compose)"
run docker exec "$C1" "$BIN" gen-key /data/backup.key
check "gen-key /data/backup.key" test "$RC" -eq 0
run docker exec "$C1" "$BIN" backup /data/court.tcrb /data/backup.key
check "backup /data/court.tcrb" test "$RC" -eq 0 -a -n "$(contains "Installation: $INSTALL_ID" && contains 'Backup created' && echo y)"
BACKUP_SUMMARY=$(printf '%s\n' "$OUT" | grep 'Backup created')
run docker exec "$C1" "$BIN" verify-audit
AUDIT1=$(printf '%s\n' "$OUT" | grep 'Audit chain')
check "verify-audit on instance 1: $AUDIT1" test "$RC" -eq 0 -a -n "$(contains 'Audit chain intact' && echo y)"
# The key vault and the separate archive storage are simulated by two private local directories
# (the docs' `scp … backup-custodian@key-vault:` hop is the only step not executed).
VAULT="$WORK/key-vault"; ARCHIVE="$WORK/archive-storage"; STAGE="$WORK/staging"
(umask 077; mkdir -p "$VAULT" "$ARCHIVE" "$STAGE")
log "+ (umask 077; docker cp $C1:/data/backup.key ./backup.key)   # docs: docker compose cp court:/data/backup.key ./backup.key"
(umask 077; docker cp "$C1:/data/backup.key" "$STAGE/backup.key" >/dev/null 2>&1)
cp -p "$STAGE/backup.key" "$VAULT/backup.key"   # docs: scp ./backup.key backup-custodian@key-vault:…
KEY_SHA=$(docker exec "$C1" sha256sum /data/backup.key | cut -c1-64)
check "vault copy of the key verified (sha256 equals the volume copy, mode $(stat -f %Lp "$VAULT/backup.key" 2>/dev/null || stat -c %a "$VAULT/backup.key"))" \
  test "$(shasum -a 256 "$VAULT/backup.key" | cut -c1-64)" = "$KEY_SHA"
run docker exec "$C1" /bin/rm /data/backup.key
check "docker exec court /bin/rm /data/backup.key" test "$RC" -eq 0
rm "$STAGE/backup.key"
check "key no longer on the data volume or in local staging" \
  sh -c "! docker exec '$C1' test -e /data/backup.key && ! test -e '$STAGE/backup.key'"
run docker cp "$C1:/data/court.tcrb" "$ARCHIVE/court.tcrb"
check "encrypted archive copied to separate storage" test -s "$ARCHIVE/court.tcrb"
log "    archive sha256: $(shasum -a 256 "$ARCHIVE/court.tcrb" | cut -c1-64)"

# --------------------------------------------------------------------------------------------
step "Negative: restore without --yes (non-interactive) and with a wrong key refuse"
# Disaster recovery into a NEW volume: archive and vault key are bind-mounted read-only, so the key
# never lands on any data volume. Docker Desktop maps bind-mount ownership to the container user;
# on a Linux host the files must be readable by uid 10001 (court).
docker volume create "$VOL2" >/dev/null
DR_MOUNTS=(-v "$ARCHIVE":/archive:ro -v "$VAULT":/vault:ro)
run docker run --rm -i -v "$VOL2":/data "${DR_MOUNTS[@]}" "${PROD_ENV[@]}" "$IMAGE" restore /archive/court.tcrb /vault/backup.key /data </dev/null
check "restore without --yes refused (exit $RC)" test "$RC" -ne 0 -a -n "$(contains 'requires --yes' && echo y)"
check "new volume still empty after refusal (listing: '$(volume_listing "$VOL2")')" volume_empty "$VOL2"
(umask 077; mkdir -p "$WORK/wrong") && docker run --rm -v "$WORK/wrong":/w "$IMAGE" gen-key /w/wrong.key >/dev/null 2>&1
run docker run --rm -v "$VOL2":/data "${DR_MOUNTS[@]}" -v "$WORK/wrong":/w:ro "${PROD_ENV[@]}" "$IMAGE" restore /archive/court.tcrb /w/wrong.key /data --yes
check "restore with a wrong key refused (exit $RC)" test "$RC" -ne 0 -a -n "$(contains 'authentication failed' && echo y)"
check "new volume still empty after wrong key (listing: '$(volume_listing "$VOL2")')" volume_empty "$VOL2"

# --------------------------------------------------------------------------------------------
step "Restore into a NEW empty volume (restore … --yes; archive + vault key, source volume not used)"
run docker run --rm -v "$VOL2":/data "${DR_MOUNTS[@]}" "${PROD_ENV[@]}" "$IMAGE" restore /archive/court.tcrb /vault/backup.key /data --yes
check "restore exit 0, installation $INSTALL_ID" test "$RC" -eq 0 -a -n "$(contains "Installation from backup: $INSTALL_ID" && contains 'Backup restored' && echo y)"
log "    volume listing: $(volume_listing "$VOL2")"
run docker run --rm -v "$VOL2":/data "${PROD_ENV[@]}" "$IMAGE" verify-audit
AUDIT2=$(printf '%s\n' "$OUT" | grep 'Audit chain')
check "verify-audit on restored volume equals instance 1 ($AUDIT2)" test "$RC" -eq 0 -a "$AUDIT2" = "$AUDIT1"
check "restored volume reports the same installation id" contains "Installation: $INSTALL_ID"
check "restored volume holds no key and no .restore-tmp (listing: '$(volume_listing "$VOL2")')" \
  sh -c "! docker run --rm -v '$VOL2':/data --entrypoint ls '$IMAGE' -A /data | grep -Eq 'restore-tmp|backup.key'"

# --------------------------------------------------------------------------------------------
step "Documented Compose restore sequence: key retrieved into the running container, stop, run restore, rm key"
(umask 077; cp -p "$VAULT/backup.key" "$STAGE/backup.key")   # docs: scp backup-custodian@key-vault:… ./backup.key
run docker cp "$STAGE/backup.key" "$C1:/data/backup.key"
check "docker cp ./backup.key court:/data/backup.key" test "$RC" -eq 0
run docker exec --user root "$C1" /bin/chown court:court /data/backup.key
check "docker exec --user root court /bin/chown court:court /data/backup.key" test "$RC" -eq 0
run docker exec "$C1" /bin/chmod 0600 /data/backup.key
check "docker exec court /bin/chmod 0600 /data/backup.key" test "$RC" -eq 0
log "    $(docker exec "$C1" ls -ln /data/backup.key 2>&1)"
rm "$STAGE/backup.key"
run docker stop "$C1"
check "docker stop court" test "$RC" -eq 0
run docker run --rm -v "$VOL1":/data "${PROD_ENV[@]}" "$IMAGE" restore /data/court.tcrb /data/backup.key /data/restored --yes
check "docker run --rm court restore /data/court.tcrb /data/backup.key /data/restored --yes" \
  test "$RC" -eq 0 -a -n "$(contains "Installation from backup: $INSTALL_ID" && contains 'Backup restored' && echo y)"
run docker run --rm --entrypoint /bin/rm -v "$VOL1":/data "$IMAGE" /data/backup.key
check "docker run --rm --entrypoint /bin/rm court /data/backup.key" test "$RC" -eq 0
check "key no longer on the source volume (listing: '$(volume_listing "$VOL1")')" \
  sh -c "! docker run --rm -v '$VOL1':/data --entrypoint ls '$IMAGE' -A /data | grep -q backup.key"
run docker run --rm -v "$VOL1":/data -e TCR_DATA_DIR=/data/restored "${PROD_ENV[@]}" "$IMAGE" verify-audit
check "verify-audit with TCR_DATA_DIR=/data/restored equals instance 1" \
  test "$RC" -eq 0 -a "$(printf '%s\n' "$OUT" | grep 'Audit chain')" = "$AUDIT1" -a -n "$(contains 'Data directory: /data/restored' && echo y)"

# --------------------------------------------------------------------------------------------
step "Start a second PRODUCTION container on the restored volume"
P2=$(free_port)
run docker run -d --name "$C2" "${LIMITS[@]}" "${PROD_ENV[@]}" -p "127.0.0.1:$P2:8088" -v "$VOL2":/data "$IMAGE"
[ "$RC" -eq 0 ] || abort "docker run $C2"
if wait_health "$P2" "$C2"; then pass "restored instance healthy on port $P2"; else docker logs "$C2" 2>&1 | tail -20; abort "restored instance not healthy"; fi
check "image HEALTHCHECK reports healthy (restored)" health_status "$C2"

# --------------------------------------------------------------------------------------------
step "HTTP client: fresh logins on the restored instance and full comparison"
log "+ python3 scripts/clean_install_check.py phase2 <state> http://127.0.0.1:$P2"
python3 "$HELPER" phase2 "$WORK/state.json" "http://127.0.0.1:$P2" | sed 's/^/    /'
PH2=${PIPESTATUS[0]}
check "phase 2 restore comparison (see indented lines)" test "$PH2" -eq 0
STATS2=$(stats "$C2")
log "docker stats (restored instance, after phase 2): $STATS2"
run docker exec "$C2" "$BIN" verify-audit
check "verify-audit on running restored instance after new activity" test "$RC" -eq 0 -a -n "$(contains 'Audit chain intact' && echo y)"

log ""
log "facts: image=$IMAGE size=$((SIZE / 1000000))MB installation=$INSTALL_ID"
log "facts: $BACKUP_SUMMARY | instance1 $AUDIT1"
log "facts: stats1: $STATS1"
log "facts: stats2: $STATS2"
[ "$FAILS" -eq 0 ]
