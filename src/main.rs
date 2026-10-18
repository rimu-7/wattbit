use std::process::ExitCode;

use anyhow::anyhow;
use clap::Parser;

use berry::cli::{Cli, Command, DaemonCommand, SpeedCommand, Switch, Toggle};
use berry::client::{render_status, Client, ClientError};
use berry::daemon::{self, DaemonOptions};
use berry::protocol::{Call, ErrorKind, Reply};
use berry::speed::agent::{AgentOptions, SpeedPaths};
use berry::speed::{agent, render, status};
use berry::{doctor, service};

/// Exit status when the hardware does not support the request.
const EXIT_UNSUPPORTED: u8 = 3;
/// Exit status when the caller lacks the required privileges.
const EXIT_PERMISSION: u8 = 4;
/// Exit status when another charge limiter controls the hardware.
const EXIT_CONFLICT: u8 = 5;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("berry: {e:#}");
            let code = match e.downcast_ref::<ClientError>() {
                Some(ClientError::Rpc(rpc)) => match rpc.kind() {
                    ErrorKind::Unsupported => EXIT_UNSUPPORTED,
                    ErrorKind::Unauthorized => EXIT_PERMISSION,
                    ErrorKind::Conflict => EXIT_CONFLICT,
                    _ => 1,
                },
                Some(ClientError::PermissionDenied { .. }) => EXIT_PERMISSION,
                _ => 1,
            };
            ExitCode::from(code)
        }
    }
}

fn client_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn request(client: &Client, call: Call) -> anyhow::Result<Reply> {
    Ok(client_runtime()?.block_on(client.call(call))?)
}

fn acknowledge(client: &Client, call: Call) -> anyhow::Result<ExitCode> {
    match request(client, call)? {
        Reply::Ack { message } => {
            println!("{message}");
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(anyhow!("unexpected reply from the daemon")),
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let client = Client::new(&cli.socket);
    match cli.command {
        Command::Status { json } => match request(&client, Call::Status)? {
            Reply::Status(report) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print!("{}", render_status(&report));
                }
                Ok(ExitCode::SUCCESS)
            }
            _ => Err(anyhow!("unexpected reply from the daemon")),
        },
        Command::Limit(args) => {
            let limit = args.resolve().map_err(|e| anyhow!(e))?;
            acknowledge(&client, limit.into_call())
        }
        Command::Adapter { action } => acknowledge(
            &client,
            Call::SetAdapter {
                enabled: action == Toggle::Enable,
            },
        ),
        Command::Set { option, value } => acknowledge(
            &client,
            Call::SetOption {
                option,
                enabled: value == Switch::On,
            },
        ),
        Command::Doctor => {
            let checks = client_runtime()?.block_on(doctor::run(&cli.socket));
            print!("{}", doctor::render(&checks));
            Ok(if doctor::has_failures(&checks) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Speed { command } => speed_command(command),
        Command::Daemon {
            command: DaemonCommand::Run { config, read_only },
        } => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            runtime.block_on(daemon::run(DaemonOptions {
                socket: cli.socket,
                config,
                read_only,
            }))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::InstallService => {
            println!("{}", service::install()?);
            Ok(ExitCode::SUCCESS)
        }
        Command::UninstallService { purge } => {
            println!("{}", service::uninstall(purge)?);
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Routes `berry speed`; none of it needs the daemon or root.
fn speed_command(command: SpeedCommand) -> anyhow::Result<ExitCode> {
    let paths = SpeedPaths::resolve()?;
    match command {
        SpeedCommand::On { no_launchd } => {
            println!("{}", agent::on(&paths, no_launchd)?);
            Ok(ExitCode::SUCCESS)
        }
        SpeedCommand::Off => {
            println!("{}", agent::off(&paths)?);
            Ok(ExitCode::SUCCESS)
        }
        SpeedCommand::Status { json } => {
            let report = status::query(&paths);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render::render(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        SpeedCommand::Agent {
            headless,
            interval,
            once,
        } => {
            if once {
                let live = agent::sample_once(&paths, interval)?;
                println!("{}", serde_json::to_string_pretty(&live)?);
            } else {
                agent::run(&paths, AgentOptions { interval, headless })?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
