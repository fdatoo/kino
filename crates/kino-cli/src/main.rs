use std::{env, process::ExitCode};

use reqwest::{StatusCode, Url};
use serde::Deserialize;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let (options, command) = parse_options(args)?;
    let client = AdminClient::new(options)?;

    match command {
        Command::Transcode(command) => run_transcode(&client, command).await,
    }
}

async fn run_transcode(client: &AdminClient, command: TranscodeCommand) -> Result<()> {
    match command {
        TranscodeCommand::Jobs(JobsCommand::List(filters)) => print_json(
            client
                .get_json("api/v1/admin/transcodes/jobs", &filters)
                .await?,
        ),
        TranscodeCommand::Jobs(JobsCommand::Show { id }) => print_json(
            client
                .get_json(&format!("api/v1/admin/transcodes/jobs/{id}"), &[])
                .await?,
        ),
        TranscodeCommand::Cancel { job_id } => print_json(
            client
                .post_json(&format!("api/v1/admin/transcodes/jobs/{job_id}/cancel"))
                .await?,
        ),
        TranscodeCommand::Retry { job_id } => {
            let detail: JobDetail = client
                .get_json(&format!("api/v1/admin/transcodes/jobs/{job_id}"), &[])
                .await?;
            print_json(
                client
                    .post_json(&format!(
                        "api/v1/admin/transcodes/sources/{}/replan",
                        detail.source_file_id
                    ))
                    .await?,
            )
        }
        TranscodeCommand::Retranscode { source_file_id } => print_json(
            client
                .post_json(&format!(
                    "api/v1/admin/transcodes/sources/{source_file_id}/retranscode"
                ))
                .await?,
        ),
        TranscodeCommand::Encoders => print_json(
            client
                .get_json("api/v1/admin/transcodes/encoders", &[])
                .await?,
        ),
    }
}

#[derive(Debug)]
struct CliOptions {
    base_url: Url,
    token: String,
}

#[derive(Debug)]
enum Command {
    Transcode(TranscodeCommand),
}

#[derive(Debug)]
enum TranscodeCommand {
    Jobs(JobsCommand),
    Cancel { job_id: String },
    Retry { job_id: String },
    Retranscode { source_file_id: String },
    Encoders,
}

#[derive(Debug)]
enum JobsCommand {
    List(Vec<(&'static str, String)>),
    Show { id: String },
}

#[derive(Debug, Deserialize)]
struct JobDetail {
    source_file_id: String,
}

struct AdminClient {
    http: reqwest::Client,
    base_url: Url,
    token: String,
}

impl AdminClient {
    fn new(options: CliOptions) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::new(),
            base_url: options.base_url,
            token: options.token,
        })
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&'static str, String)],
    ) -> Result<T> {
        let url = self.url(path)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await?;

        decode_response(response).await
    }

    async fn post_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .post(self.url(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;

        decode_response(response).await
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(Error::Url)
    }
}

async fn decode_response<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let bytes = response.bytes().await?;
    if status.is_success() {
        return Ok(serde_json::from_slice(&bytes)?);
    }

    let message = serde_json::from_slice::<ApiErrorResponse>(&bytes)
        .map(|body| body.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
    Err(Error::Http { status, message })
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: String,
}

fn parse_options(args: Vec<String>) -> Result<(CliOptions, Command)> {
    let mut base_url = env::var("KINO_SERVER_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
    let mut token = env::var("KINO_ADMIN_TOKEN").ok();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--base-url" => {
                index += 1;
                base_url = args
                    .get(index)
                    .cloned()
                    .ok_or(Error::MissingOptionValue("--base-url"))?;
            }
            "--token" => {
                index += 1;
                token = Some(
                    args.get(index)
                        .cloned()
                        .ok_or(Error::MissingOptionValue("--token"))?,
                );
            }
            "--help" | "-h" => return Err(Error::Usage(usage())),
            _ => break,
        }
        index += 1;
    }

    let Some(token) = token else {
        return Err(Error::MissingToken);
    };
    let command = parse_command(&args[index..])?;
    let mut base_url = Url::parse(&base_url)?;
    if !base_url.path().ends_with('/') {
        base_url.set_path(&format!("{}/", base_url.path()));
    }

    Ok((CliOptions { base_url, token }, command))
}

fn parse_command(args: &[String]) -> Result<Command> {
    match args.first().map(String::as_str) {
        Some("transcode") => Ok(Command::Transcode(parse_transcode(&args[1..])?)),
        _ => Err(Error::Usage(usage())),
    }
}

fn parse_transcode(args: &[String]) -> Result<TranscodeCommand> {
    match args.first().map(String::as_str) {
        Some("jobs") => Ok(TranscodeCommand::Jobs(parse_jobs(&args[1..])?)),
        Some("cancel") => Ok(TranscodeCommand::Cancel {
            job_id: required_arg(args, 1, "job_id")?,
        }),
        Some("retry") => Ok(TranscodeCommand::Retry {
            job_id: required_arg(args, 1, "job_id")?,
        }),
        Some("retranscode") => Ok(TranscodeCommand::Retranscode {
            source_file_id: required_arg(args, 1, "source_file_id")?,
        }),
        Some("encoders") if args.len() == 1 => Ok(TranscodeCommand::Encoders),
        _ => Err(Error::Usage(usage())),
    }
}

fn parse_jobs(args: &[String]) -> Result<JobsCommand> {
    match args.first().map(String::as_str) {
        None | Some("list") => Ok(JobsCommand::List(parse_job_filters(args)?)),
        Some("show") => Ok(JobsCommand::Show {
            id: required_arg(args, 1, "id")?,
        }),
        _ => Err(Error::Usage(usage())),
    }
}

fn parse_job_filters(args: &[String]) -> Result<Vec<(&'static str, String)>> {
    let mut filters = Vec::new();
    let mut index = usize::from(args.first().is_some_and(|arg| arg == "list"));
    while index < args.len() {
        let key = match args[index].as_str() {
            "--state" => "state",
            "--lane" => "lane",
            "--source-file-id" => "source_file_id",
            _ => return Err(Error::Usage(usage())),
        };
        index += 1;
        let value = args
            .get(index)
            .cloned()
            .ok_or(Error::MissingOptionValue(match key {
                "state" => "--state",
                "lane" => "--lane",
                _ => "--source-file-id",
            }))?;
        filters.push((key, value));
        index += 1;
    }
    Ok(filters)
}

fn required_arg(args: &[String], index: usize, name: &'static str) -> Result<String> {
    let value = args
        .get(index)
        .cloned()
        .ok_or(Error::MissingArgument(name))?;
    if args.len() != index + 1 {
        return Err(Error::Usage(usage()));
    }
    Ok(value)
}

fn print_json(value: serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn usage() -> String {
    concat!(
        "usage: kino-cli [--base-url URL] [--token TOKEN] transcode <command>\n",
        "commands:\n",
        "  transcode jobs [list] [--state STATE] [--lane LANE] [--source-file-id ID]\n",
        "  transcode jobs show <id>\n",
        "  transcode cancel <job_id>\n",
        "  transcode retry <job_id>\n",
        "  transcode retranscode <source_file_id>\n",
        "  transcode encoders"
    )
    .to_owned()
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{0}")]
    Usage(String),

    #[error("missing required argument: {0}")]
    MissingArgument(&'static str),

    #[error("missing value for option: {0}")]
    MissingOptionValue(&'static str),

    #[error("admin token is required; pass --token or set KINO_ADMIN_TOKEN")]
    MissingToken,

    #[error(transparent)]
    Request(#[from] reqwest::Error),

    #[error(transparent)]
    Url(#[from] url::ParseError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("admin request failed with {status}: {message}")]
    Http { status: StatusCode, message: String },
}
