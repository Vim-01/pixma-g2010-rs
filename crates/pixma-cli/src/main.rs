//! Entry point for `pixma-cli`.

use clap::Parser;
use pixma_cli::args::{Cli, Commands};
use pixma_cli::commands::{align, clean, print, roller_clean, scan};
use pixma_cli::connect_client;
use pixma_cli::status::render_ascii_status;
use pixma_core::maintenance::{MaintenanceAction, TestPrintType};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    let cli = Cli::parse();
    let mut client = connect_client(cli.socket.as_deref(), cli.mock).await?;

    match cli.command {
        Commands::Status => {
            let status = client.get_status().await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", render_ascii_status(&status));
            }
        }
        Commands::Clean {
            kind,
            target,
            confirm,
            override_cooldown_dangerously,
        } => {
            clean::execute_clean(
                &mut client,
                kind,
                target,
                confirm.as_deref(),
                override_cooldown_dangerously,
                cli.json,
            )
            .await?;
        }
        Commands::RollerClean { target } => {
            roller_clean::execute_roller_clean(&mut client, target, cli.json).await?;
        }
        Commands::NozzleCheck => {
            if !cli.json {
                println!("Printing diagnostic nozzle check pattern...");
            }
            let action = MaintenanceAction::TestPrint(TestPrintType::NozzleCheck);
            let status = client.execute_maintenance(action, None, false).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Nozzle check pattern command successfully sent to printer.");
            }
        }
        Commands::Align { action, values } => {
            align::execute_align(&mut client, action, values.as_deref(), cli.json).await?;
        }
        Commands::Scan {
            dpi,
            mode,
            output,
            width,
            height,
            auto_crop,
            deskew,
            descreen,
            unsharp,
            remove_dust,
            gutter_shadow,
            gamma,
        } => {
            scan::execute_scan(
                &mut client,
                dpi,
                mode,
                &output,
                width,
                height,
                auto_crop,
                deskew,
                descreen,
                unsharp,
                remove_dust,
                gutter_shadow,
                gamma,
                cli.json,
            )
            .await?;
        }
        Commands::Print {
            input,
            test_page,
            media,
            paper,
            quality,
            borderless,
            layout,
            grayscale,
            cyan,
            magenta,
            yellow,
            brightness,
            contrast,
            wide_gap,
            drying,
            quiet,
        } => {
            print::execute_print(
                &mut client,
                input.as_deref(),
                test_page,
                media,
                paper,
                quality,
                borderless,
                layout,
                grayscale,
                cyan,
                magenta,
                yellow,
                brightness,
                contrast,
                wide_gap,
                drying,
                quiet,
                cli.json,
            )
            .await?;
        }
    }

    Ok(())
}
