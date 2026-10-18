use std::path::PathBuf;

use berry::cli::{
    parse_limit_target, Cli, Command, DaemonCommand, LimitRequest, LimitTarget, SpeedCommand,
    Switch, Toggle,
};
use berry::config::ConfigOption;
use berry::policy::Thresholds;
use berry::protocol::Call;
use berry::{CONFIG_PATH, SOCKET_PATH};
use clap::{CommandFactory, Parser};

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("berry").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("{args:?} should parse: {e}"))
}

fn rejects(args: &[&str]) {
    assert!(
        Cli::try_parse_from(std::iter::once("berry").chain(args.iter().copied())).is_err(),
        "{args:?} should be rejected"
    );
}

fn limit(args: &[&str]) -> Result<LimitRequest, String> {
    match parse(args).command {
        Command::Limit(limit) => limit.resolve(),
        other => panic!("expected limit, got {other:?}"),
    }
}

#[test]
fn clap_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn status_defaults_to_system_socket() {
    let cli = parse(&["status"]);
    assert_eq!(cli.socket, PathBuf::from(SOCKET_PATH));
    assert!(matches!(cli.command, Command::Status { json: false }));
    assert!(matches!(
        parse(&["status", "--json"]).command,
        Command::Status { json: true }
    ));
}

#[test]
fn socket_is_a_global_option() {
    let cli = parse(&["status", "--socket", "/tmp/b.sock"]);
    assert_eq!(cli.socket, PathBuf::from("/tmp/b.sock"));
}

#[test]
fn limit_defaults_to_80_75() {
    assert_eq!(
        limit(&["limit"]),
        Ok(LimitRequest::Set(Thresholds::new(80, Some(75)).unwrap()))
    );
}

#[test]
fn limit_derives_lower_with_hysteresis() {
    assert_eq!(
        limit(&["limit", "90"]),
        Ok(LimitRequest::Set(Thresholds::new(90, Some(85)).unwrap()))
    );
    assert_eq!(
        limit(&["limit", "60%"]),
        Ok(LimitRequest::Set(Thresholds::new(60, Some(55)).unwrap()))
    );
}

#[test]
fn limit_accepts_explicit_lower() {
    assert_eq!(
        limit(&["limit", "80", "--lower", "60"]),
        Ok(LimitRequest::Set(Thresholds::new(80, Some(60)).unwrap()))
    );
    assert!(limit(&["limit", "80", "--lower", "80"]).is_err());
    assert!(limit(&["limit", "80", "--lower", "95"]).is_err());
}

#[test]
fn limit_disable() {
    assert_eq!(limit(&["limit", "disable"]), Ok(LimitRequest::Disable));
    assert_eq!(limit(&["limit", "DISABLE"]), Ok(LimitRequest::Disable));
    assert!(limit(&["limit", "disable", "--lower", "50"]).is_err());
    assert_eq!(LimitRequest::Disable.into_call(), Call::DisableLimit);
}

#[test]
fn limit_rejects_out_of_range_values() {
    rejects(&["limit", "49"]);
    rejects(&["limit", "101"]);
    rejects(&["limit", "-5"]);
    rejects(&["limit", "eighty"]);
    rejects(&["limit", "80", "--lower", "300"]);
}

#[test]
fn limit_target_parser() {
    assert_eq!(parse_limit_target("100"), Ok(LimitTarget::Percent(100)));
    assert_eq!(parse_limit_target("50%"), Ok(LimitTarget::Percent(50)));
    assert!(parse_limit_target("").is_err());
    assert!(parse_limit_target("%").is_err());
}

#[test]
fn limit_request_maps_to_call() {
    let request = LimitRequest::Set(Thresholds::new(85, None).unwrap());
    assert_eq!(
        request.into_call(),
        Call::SetLimit {
            upper: 85,
            lower: 80
        }
    );
}

#[test]
fn adapter_commands() {
    assert!(matches!(
        parse(&["adapter", "enable"]).command,
        Command::Adapter {
            action: Toggle::Enable
        }
    ));
    assert!(matches!(
        parse(&["adapter", "disable"]).command,
        Command::Adapter {
            action: Toggle::Disable
        }
    ));
    rejects(&["adapter"]);
    rejects(&["adapter", "toggle"]);
}

#[test]
fn set_options() {
    match parse(&["set", "magsafe-led", "on"]).command {
        Command::Set { option, value } => {
            assert_eq!(option, ConfigOption::MagsafeLed);
            assert_eq!(value, Switch::On);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        parse(&["set", "adapter-fallback", "off"]).command,
        Command::Set {
            option: ConfigOption::AdapterFallback,
            value: Switch::Off
        }
    ));
    rejects(&["set", "turbo", "on"]);
    rejects(&["set", "magsafe-led", "maybe"]);
}

#[test]
fn daemon_run_options() {
    match parse(&["daemon", "run"]).command {
        Command::Daemon {
            command: DaemonCommand::Run { config, read_only },
        } => {
            assert_eq!(config, PathBuf::from(CONFIG_PATH));
            assert!(!read_only);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse(&["daemon", "run", "--read-only", "--config", "/tmp/c.json"]).command {
        Command::Daemon {
            command: DaemonCommand::Run { config, read_only },
        } => {
            assert_eq!(config, PathBuf::from("/tmp/c.json"));
            assert!(read_only);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn service_commands() {
    assert!(matches!(
        parse(&["install-service"]).command,
        Command::InstallService
    ));
    assert!(matches!(
        parse(&["uninstall-service"]).command,
        Command::UninstallService { purge: false }
    ));
    assert!(matches!(
        parse(&["uninstall-service", "--purge"]).command,
        Command::UninstallService { purge: true }
    ));
    assert!(matches!(parse(&["doctor"]).command, Command::Doctor));
}

#[test]
fn speed_commands() {
    assert!(matches!(
        parse(&["speed", "on"]).command,
        Command::Speed {
            command: SpeedCommand::On { no_launchd: false }
        }
    ));
    assert!(matches!(
        parse(&["speed", "on", "--no-launchd"]).command,
        Command::Speed {
            command: SpeedCommand::On { no_launchd: true }
        }
    ));
    assert!(matches!(
        parse(&["speed", "off"]).command,
        Command::Speed {
            command: SpeedCommand::Off
        }
    ));
    assert!(matches!(
        parse(&["speed", "status"]).command,
        Command::Speed {
            command: SpeedCommand::Status { json: false }
        }
    ));
    assert!(matches!(
        parse(&["speed", "status", "--json"]).command,
        Command::Speed {
            command: SpeedCommand::Status { json: true }
        }
    ));
    rejects(&["speed"]);
    rejects(&["speed", "toggle"]);
}

#[test]
fn speed_agent_options() {
    match parse(&["speed", "agent"]).command {
        Command::Speed {
            command:
                SpeedCommand::Agent {
                    headless,
                    interval,
                    once,
                },
        } => {
            assert!(!headless);
            assert_eq!(interval, 1.0);
            assert!(!once);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse(&[
        "speed",
        "agent",
        "--headless",
        "--interval",
        "2.5",
        "--once",
    ])
    .command
    {
        Command::Speed {
            command:
                SpeedCommand::Agent {
                    headless,
                    interval,
                    once,
                },
        } => {
            assert!(headless);
            assert_eq!(interval, 2.5);
            assert!(once);
        }
        other => panic!("unexpected {other:?}"),
    }
    rejects(&["speed", "agent", "--interval", "0.01"]);
    rejects(&["speed", "agent", "--interval", "61"]);
    rejects(&["speed", "agent", "--interval", "fast"]);
    rejects(&["speed", "agent", "--interval", "nan"]);
}
