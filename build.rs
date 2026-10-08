// The SPA is embedded from web/dist. Make sure the folder exists so a fresh clone compiles even
// before `npm run build`; the server then explains that the frontend is not built.
fn main() {
    std::fs::create_dir_all("web/dist").expect("create web/dist");
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=src/migrations");
}
