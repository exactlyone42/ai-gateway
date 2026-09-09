fn main() {
    // `sqlx::migrate!` reads the migrations directory at COMPILE time. Without
    // this line an incremental build reuses the old macro expansion, so a new
    // .sql file is silently absent from the binary -- measured 2026-09-10: a
    // deliberately broken migration was ignored and the process started
    // normally. Deployment traps do not get quieter than that.
    println!("cargo:rerun-if-changed=migrations");
}
