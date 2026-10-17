//! Render clap's syntax metadata with localized labels and descriptions.
//!
//! Clap still parses the command and selects the appropriate help page. Owning
//! its presentation avoids untranslated framework headings and diagnostic text.

use clap::Command;
use superba_core::Catalog;

use crate::text::{text, translate_help};

pub(crate) fn localized(mut command: Command, catalog: &Catalog) -> Command {
    command.build();
    let help = render(&mut command, catalog);
    command
        .override_help(help)
        .mut_subcommands(|sub| localized(sub, catalog))
}

fn about(command: &Command, catalog: &Catalog) -> String {
    if command.get_name() == "help" {
        catalog.text(&text("cli.help.help_command"))
    } else {
        command
            .get_about()
            .map(|value| translate_help(catalog, &value.to_string()))
            .unwrap_or_default()
    }
}

fn render(command: &mut Command, catalog: &Catalog) -> String {
    let mut result = about(command, catalog);
    result.push_str("\n\n");
    let usage = command.render_usage().to_string();
    let syntax = usage
        .strip_prefix("Usage: ")
        .unwrap_or(&usage)
        .replace("[OPTIONS]", &catalog.text(&text("cli.help.options_token")))
        .replace(
            "<COMMAND>",
            &catalog.text(&text("cli.help.command_required")),
        )
        .replace(
            "[COMMAND]",
            &catalog.text(&text("cli.help.command_optional")),
        );
    result.push_str(&catalog.text(&text("cli.help.usage_line").arg("usage", syntax)));
    result.push('\n');
    if command.get_name() == "superba" {
        result.push_str(&catalog.text(&text("cli.help.gui_options")));
        result.push('\n');
    }
    let subcommands = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .collect::<Vec<_>>();
    if !subcommands.is_empty() {
        heading(&mut result, catalog, "cli.help.commands");
        for sub in subcommands {
            let mut description = about(sub, catalog);
            let aliases = sub.get_visible_aliases().collect::<Vec<_>>();
            if !aliases.is_empty() {
                description.push(' ');
                description.push_str(
                    &catalog.text(&text("cli.help.aliases").arg("aliases", aliases.join(", "))),
                );
            }
            row(&mut result, sub.get_name(), &description);
        }
    }
    for (positional, key) in [(true, "cli.help.arguments"), (false, "cli.help.options")] {
        let args = command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set() && arg.is_positional() == positional)
            .collect::<Vec<_>>();
        if args.is_empty() {
            continue;
        }
        heading(&mut result, catalog, key);
        for arg in args {
            let mut description = match arg.get_id().as_str() {
                "help" => catalog.text(&text("cli.help.help")),
                "version" => catalog.text(&text("cli.help.version")),
                _ if command.get_name() == "help" => catalog.text(&text("cli.help.help_command")),
                _ => arg
                    .get_help()
                    .map(|help| translate_help(catalog, &help.to_string()))
                    .unwrap_or_default(),
            };
            let values = arg
                .get_possible_values()
                .into_iter()
                .filter(|v| !v.is_hide_set())
                .map(|v| v.get_name().to_owned())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                description.push(' ');
                description.push_str(
                    &catalog.text(&text("cli.help.values").arg("values", values.join(", "))),
                );
            }
            let defaults = arg
                .get_default_values()
                .iter()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>();
            if !defaults.is_empty() && arg.get_action().takes_values() {
                description.push(' ');
                description.push_str(
                    &catalog
                        .text(&text("cli.help.default_value").arg("value", defaults.join(", "))),
                );
            }
            if let Some(aliases) = arg.get_visible_aliases() {
                description.push(' ');
                let aliases = aliases
                    .iter()
                    .map(|alias| format!("--{alias}"))
                    .collect::<Vec<_>>();
                description.push_str(
                    &catalog.text(&text("cli.help.aliases").arg("aliases", aliases.join(", "))),
                );
            }
            let mut name = arg.to_string();
            if let (Some(short), Some(_)) = (arg.get_short(), arg.get_long()) {
                name = format!("-{short}, {name}");
            }
            row(&mut result, &name, &description);
        }
    }
    result
}

fn heading(result: &mut String, catalog: &Catalog, key: &str) {
    result.push('\n');
    result.push_str(&catalog.text(&text(key)));
    result.push_str(":\n");
}

fn row(result: &mut String, name: &str, description: &str) {
    let padding = 28_usize.saturating_sub(crate::output::display_width(name));
    result.push_str(&format!("  {name}{} {description}\n", " ".repeat(padding)));
}
