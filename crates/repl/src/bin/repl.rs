// use clap::{CommandFactory, Parser};
// use repl::{
// 	AgentCli, HostReplApplication, OutputStyles, info,
// 	output::{OutputFormat, initialize_output_config},
// };

pub fn print_completions<G: clap_complete::Generator>(generator: G, cmd: &mut clap::Command) {
	clap_complete::generate(
		generator,
		cmd,
		cmd.get_name().to_string(),
		&mut std::io::stdout(),
	);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	/* 	let cli = AgentCli::parse();

	   let default_level = cli.globals.verbosity.log_level_filter();
	   let crate_name = env!("CARGO_PKG_NAME").replace('-', "_");
	   let binary_name = env!("CARGO_BIN_NAME").replace('-', "_");
	   let filter = tracing_subscriber::EnvFilter::from_default_env()
		   .add_directive(format!("wallhack={default_level}").parse()?)
		   .add_directive(format!("{crate_name}={default_level}").parse()?)
		   .add_directive(format!("{binary_name}={default_level}").parse()?);

	   tracing_subscriber::fmt()
		   .compact()
		   .with_env_filter(filter)
		   .with_file(false)
		  //  .with_thread_ids(true)
		   .with_target(true)
		   .init();

	   initialize_output_config(OutputFormat::Plain, OutputStyles::default(), true);

	   info!("LOL");

	   // Handle completions generation (same as before)
	   if let Some(generator) = cli.globals.generator {
		   let mut cmd = AgentCli::command();
		   print_completions(generator, &mut cmd);
		   return Ok(());
	   }

	   // TODO: Add XDG base directory support
	   let mut repl_app = HostReplApplication::new(cli)?;
	   repl_app.run()?;
	*/
	Ok(())
}
