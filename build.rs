fn main() {
    // `sqlx::migrate!` reads the migrations directory at COMPILE time. Without
    // this line an incremental build reuses the old macro expansion, so a new
    // .sql file is silently absent from the binary -- measured 2026-09-10: a
    // deliberately broken migration was ignored and the process started
    // normally. Deployment traps do not get quieter than that.
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=queries");
    println!("cargo:rerun-if-changed=.sqlx");

    // A deployment has to be able to answer "is the running process the build
    // I just pushed?" without anyone's word for it. The image build has no
    // .git directory, so the identifier arrives as a build argument; a local
    // build reads git directly, and a build with neither says so.
    println!("cargo:rerun-if-env-changed=BUILD_REVISION");
    let revision = std::env::var("BUILD_REVISION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=BUILD_REVISION={revision}");
}
