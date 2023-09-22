mod handlers;
mod repo;
mod help;
mod metrics;

use std::env::VarError;
use std::net::SocketAddr;
use axum::Router;
use reqwest::Url;
use rust_i18n::i18n;
use teloxide::prelude::*;
use dotenvy::dotenv;
use refinery::config::Config;
use teloxide::dptree::deps;
use teloxide::update_listeners::webhooks::{axum_to_router, Options};
use crate::handlers::{DickCommands, HelpCommands};


const ENV_WEBHOOK_URL: &str = "WEBHOOK_URL";

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!();
}

i18n!();    // load localizations with default parameters

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    dotenv()?;

    let handler = dptree::entry()
        .branch(Update::filter_message().filter_command::<HelpCommands>().endpoint(handlers::help_cmd_handler))
        .branch(Update::filter_message().filter_command::<DickCommands>().endpoint(handlers::dick_cmd_handler));
        // TODO: inline mode
        //.branch(Update::filter_inline_query().endpoint(handlers::inline_handler))
        //.branch(Update::filter_chosen_inline_result().endpoint(handlers::inline_chosen_handler))
        //.branch(Update::filter_callback_query().endpoint(handlers::callback_handler));

    let bot = Bot::from_env();
    bot.delete_webhook().await?;

    let webhook_url: Option<Url> = match std::env::var(ENV_WEBHOOK_URL) {
        Ok(env_url) if env_url.len() > 0 => Some(env_url.parse()?),
        Ok(env_url) if env_url.len() == 0 => None,
        Err(VarError::NotPresent) => None,
        _ => Err("invalid webhook URL!")?
    };
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let metrics_router = metrics::init();

    run_migrations().await?;
    let db_conn = establish_database_connection().await?;
    let deps = deps![
        repo::Users::new(db_conn.clone()),
        repo::Dicks::new(db_conn)
    ];

    match webhook_url {
        Some(url) => {
            log::info!("Setting a webhook: {url}");

            let (listener, stop_flag, bot_router) = axum_to_router(bot.clone(), Options::new(addr, url)).await?;

            let error_handler = LoggingErrorHandler::with_custom_text("An error from the update listener");
            let mut dispatcher = Dispatcher::builder(bot, handler)
                .dependencies(deps)
                .build();
            let bot_fut = dispatcher.dispatch_with_listener(listener, error_handler);

            let srv = tokio::spawn(async move {
                axum::Server::bind(&addr)
                    .serve(Router::new()
                        .merge(metrics_router)
                        .merge(bot_router)
                        .into_make_service())
                    .with_graceful_shutdown(stop_flag)
                    .await
            }
            );

            let (res, _) = futures::join!(srv, bot_fut);
            res?.map_err(|e| e.into()).into()
        }
        None => {
            log::info!("The polling dispatcher is activating...");

            let bot_fut = tokio::spawn(async move {
                Dispatcher::builder(bot, handler)
                    .dependencies(deps)
                    .enable_ctrlc_handler()
                    .build()
                    .dispatch()
                    .await
            });

            let srv = tokio::spawn(async move {
                axum::Server::bind(&addr)
                    .serve(metrics_router.into_make_service())
                    .with_graceful_shutdown(async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("failed to install CTRL+C signal handler");
                        log::info!("Shutdown of the metrics server")
                    })
                    .await
            });

            let (res, _) = futures::join!(srv, bot_fut);
            res?.map_err(|e| e.into()).into()
        }
    }
}

async fn establish_database_connection() -> Result<sqlx::Pool<sqlx::Postgres>, anyhow::Error> {
    let url = std::env::var("DATABASE_URL")?;
    let mc: u32 = std::env::var("DATABASE_MAX_CONNECTIONS")?.parse()?;
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(mc)
        .connect(url.as_str()).await
        .map_err(|e| e.into())
}

async fn run_migrations() -> anyhow::Result<()> {
    let url: Url = std::env::var("DATABASE_URL")?.parse()?;
    let mut conn = Config::try_from(url)?;
    embedded::migrations::runner().run_async(&mut conn).await?;
    Ok(())
}
