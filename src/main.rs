use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Returning the error from `main` formats it with Debug, which
            // prints the variant name and drops the message that tells an
            // operator what to change. Startup failures also happen before
            // tracing exists, so stderr is the only place they can land.
            eprintln!("agent-pool-gateway stopped with an error: {error}");
            let mut source = std::error::Error::source(error.as_ref());
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = match args.as_slice() {
        [] => runtime.block_on(agent_pool_gateway::startup::run()),
        [arg] if arg == "--initialize-content-capture" => {
            runtime.block_on(agent_pool_gateway::startup::initialize_content_capture())
        }
        _ => Err("unknown command".into()),
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
