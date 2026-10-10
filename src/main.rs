use clap::Parser;

fn main() {
    let cli = agent_cli_lab::Cli::parse();
    let code = match agent_cli_lab::execute(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agent-cli-lab: {error:#}");
            1
        }
    };
    std::process::exit(code);
}
