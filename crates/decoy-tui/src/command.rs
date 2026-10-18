//! Slash commands: the Controller verbs in Decoy vocabulary, plus a few TUI-only ones.

use decoy_runtime::Activation;

/// Every command, in the order `/help` and the completion popup list them.
pub const COMMANDS: &[Spec] = &[
    Spec {
        name: "/collection",
        args: "<id>",
        about: "switch the active Session's Collection",
    },
    Spec {
        name: "/route",
        args: "<route:case:behavior>",
        about: "override one Route's Case in the active Session",
    },
    Spec {
        name: "/reset",
        args: "",
        about: "clear the active Session's overrides, keep its Collection",
    },
    Spec {
        name: "/session",
        args: "<id>",
        about: "switch the Session commands target",
    },
    Spec {
        name: "/sessions",
        args: "",
        about: "list the Sessions that hold a Selection",
    },
    Spec {
        name: "/status",
        args: "",
        about: "show the active Session's Collection and overrides",
    },
    Spec {
        name: "/help",
        args: "",
        about: "list commands and keys",
    },
    Spec {
        name: "/quit",
        args: "",
        about: "stop the server and exit",
    },
];

pub struct Spec {
    pub name: &'static str,
    /// Empty for a command without arguments.
    pub args: &'static str,
    pub about: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Collection(String),
    Route(Activation),
    Reset,
    Session(String),
    Sessions,
    Status,
    Help,
    Quit,
}

/// Parses one submitted input line; the error is the line to print.
pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.trim();
    let Some(name) = input
        .split_whitespace()
        .next()
        .filter(|name| name.starts_with('/'))
    else {
        return Err("commands start with /; free text is reserved (try /help)".to_owned());
    };
    let arg = input[name.len()..].trim();
    let Some(spec) = COMMANDS.iter().find(|spec| spec.name == name) else {
        return Err(format!("unknown command {name} (try /help)"));
    };
    let usage = || {
        format!("usage: {} {}", spec.name, spec.args)
            .trim_end()
            .to_owned()
    };
    if spec.args.is_empty() != arg.is_empty() || arg.contains(char::is_whitespace) {
        return Err(usage());
    }
    Ok(match name {
        "/collection" => Command::Collection(arg.to_owned()),
        "/route" => Command::Route(Activation::parse(arg).map_err(|_| usage())?),
        "/reset" => Command::Reset,
        "/session" => Command::Session(arg.to_owned()),
        "/sessions" => Command::Sessions,
        "/status" => Command::Status,
        "/help" => Command::Help,
        "/quit" => Command::Quit,
        _ => unreachable!("every COMMANDS entry is parsed above"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command() {
        assert_eq!(
            parse("/collection errors"),
            Ok(Command::Collection("errors".into()))
        );
        assert_eq!(
            parse("  /route users:u1:missing "),
            Ok(Command::Route(
                Activation::parse("users:u1:missing").unwrap()
            ))
        );
        assert_eq!(parse("/reset"), Ok(Command::Reset));
        assert_eq!(
            parse("/session e2e-1"),
            Ok(Command::Session("e2e-1".into()))
        );
        assert_eq!(parse("/sessions"), Ok(Command::Sessions));
        assert_eq!(parse("/status"), Ok(Command::Status));
        assert_eq!(parse("/help"), Ok(Command::Help));
        assert_eq!(parse("/quit"), Ok(Command::Quit));
    }

    #[test]
    fn wrong_arguments_print_the_usage() {
        assert_eq!(parse("/collection"), Err("usage: /collection <id>".into()));
        assert_eq!(
            parse("/collection a b"),
            Err("usage: /collection <id>".into())
        );
        assert_eq!(parse("/reset now"), Err("usage: /reset".into()));
        assert_eq!(
            parse("/route users:u1"),
            Err("usage: /route <route:case:behavior>".into())
        );
    }

    #[test]
    fn unknown_commands_and_free_text_get_a_hint() {
        assert_eq!(
            parse("/nope"),
            Err("unknown command /nope (try /help)".into())
        );
        assert_eq!(
            parse("hello"),
            Err("commands start with /; free text is reserved (try /help)".into())
        );
    }
}
