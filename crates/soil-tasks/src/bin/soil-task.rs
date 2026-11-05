//! Create, list and complete task records with mentions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use fungos_records::Store;
use soil_tasks::{NewTask, complete_task, create_task, get_task, list_tasks, task_mentions};

#[derive(Parser)]
#[command(version, about = "Tasks over fungos-records")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a task. Body may contain @[label](locator) mentions.
    Create {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/tasks")]
        book: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        due: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        contact: Option<String>,
        #[arg(long, default_value = "private")]
        visibility: String,
    },
    /// List tasks.
    List {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/tasks")]
        book: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        owner: Option<String>,
    },
    /// Mark a task done.
    Done {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/tasks")]
        book: String,
        #[arg(long)]
        id: String,
    },
    /// Show a task and its outgoing mentions.
    Show {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/tasks")]
        book: String,
        #[arg(long)]
        id: String,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Create {
            root,
            book,
            id,
            title,
            body,
            owner,
            due,
            project,
            account,
            contact,
            visibility,
        } => {
            let root = root.unwrap_or_else(default_root);
            let mut store = open(&root)?;
            let task = create_task(
                &mut store,
                &book,
                NewTask {
                    id,
                    title,
                    body,
                    owner,
                    due_at: due,
                    project_id: project,
                    account_id: account,
                    contact_token: contact,
                    visibility: Some(visibility),
                    ..NewTask::default()
                },
            )?;
            println!("{}", task.id);
        }
        Cmd::List {
            root,
            book,
            status,
            owner,
        } => {
            let root = root.unwrap_or_else(default_root);
            let store = open(&root)?;
            for task in list_tasks(&store, &book, status.as_deref(), owner.as_deref())? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    task.id,
                    task.status,
                    task.owner.as_deref().unwrap_or("-"),
                    task.due_at.as_deref().unwrap_or("-"),
                    task.title
                );
            }
        }
        Cmd::Done { root, book, id } => {
            let root = root.unwrap_or_else(default_root);
            let mut store = open(&root)?;
            let task = complete_task(&mut store, &book, &id)?;
            println!("{} {}", task.id, task.status);
        }
        Cmd::Show { root, book, id } => {
            let root = root.unwrap_or_else(default_root);
            let store = open(&root)?;
            let task = get_task(&store, &book, &id)?;
            println!("{}", serde_json::to_string_pretty(&task)?);
            for mention in task_mentions(&store, &book, &id)? {
                println!(
                    "mention\t{}\t{}",
                    mention.target,
                    mention.title.unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

fn open(root: &std::path::Path) -> anyhow::Result<Store> {
    Store::open(root, &root.join("device_id"))
}

fn default_root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/fungos/records")
}
