use std::{env, fs};
use std::fs::File;
use std::io::{BufRead, stdin};
use std::num::NonZeroU128;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use ethers::prelude::{Http, Provider};
use ch4nn337_lib::Channel;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Open {
        #[arg(short, long, default_value_t = 5)]
        chain_id: u128,
        #[arg(short, long, default_value = "0x5ff137d4b0fdcd49dca30c7cf57e578a026d2789")]
        entry_point: String,
        #[arg(short, long, default_value = "TODO")]
        factory: String,
        name: String,
    },
    Status {
        name: String,
    },
    Deploy {
        name: String,
    },
    Request {
        name: String,
        wei: NonZeroU128,
    },
    Withdraw {
        name: String, // todo implement partial withdrawal
    },
    Receive {
        name: String,
    },
    Response {
        name: String,
    },
    Cancel {
        name: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let Ok(rpc) = env::var("ETH_RPC_URL") else {
        eprintln!("unable to read ETH_RPC_URL from env!");
        return;
    };

    let Ok(provider) = Provider::<Http>::try_from(rpc) else {
        eprintln!("unable to create provider");
        return;
    };
    let provider = Arc::new(provider);

    let mut data_dir = dirs::home_dir().unwrap();
    data_dir.push(".ch4nn337");
    if let Err(err) = fs::create_dir_all(&data_dir) {
        eprintln!("unable to create data dir: {err}");
        return;
    }

    if let Err(err) = execute(cli, provider).await {
        eprintln!("caught err: {:?}", err);
    }
}

async fn execute(cli: Cli, provider: Arc<Provider<Http>>) -> Result<(), anyhow::Error> {
    match cli.command {
        Commands::Open { chain_id, entry_point, factory, name } => {
            let Ok(entry_point) = entry_point.parse() else {
                eprintln!("entry point is not an address");
                return Ok(());
            };
            let Ok(factory) = factory.parse() else {
                eprintln!("factory is not an address");
                return Ok(());
            };

            let (a, b) = match Channel::open(chain_id.into(), entry_point, factory, provider).await {
                Ok(x) => x,
                Err(err) => {
                    eprintln!("could not open channel: {err}");
                    return Ok(());
                }
            };

            write(&format!("{name}_a"), &a);
            write(&format!("{name}_b"), &b);
            println!("{name}_a and {name}_b successfully created!");
            println!("Channel address: {:?}", a.address());
            println!("{name}_a address: {:?}", a.our_address());
            println!("{name}_b address: {:?}", b.our_address());
        }
        Commands::Status { name } => {
            let Some(channel) = read(&name) else {
                eprintln!("unable to load channel data");
                return Ok(());
            };
            let (our_balance, their_balance) = channel.get_sorted_balances(provider.clone()).await?;
            println!("{name} at {:?}", channel.address());
            println!("Us:   {:?} with balance {our_balance}", channel.our_address());
            println!("Them: {:?} with balance {their_balance}", channel.their_address());
            println!("Last nonce: {}", channel.last_nonce());
            if let Some(_) = channel.pending_message() {
                println!("Waiting for response...");
            }
            if let Some(dispute) = channel.get_dispute_info(provider).await? {
                println!("DISPUTE!");
                println!("Dispute nonce: {}", dispute.nonce);
                println!("Dispute timeout: {}", dispute.timeout);
                println!("Our dispute value: {}", dispute.withdrawal_ours);
                println!("Their dispute value: {}", dispute.withdrawal_theirs);
            } else {
                println!("No ongoing dispute :)")
            }
        }
        Commands::Deploy { name } => todo!(),
        Commands::Request { name, wei } => {
            let Some(mut channel) = read(&name) else {
                eprintln!("unable to load channel data");
                return Ok(());
            };
            let request = channel.request_transfer(wei, provider).await?;
            println!("Send this to be signed by the counterparty:\n{request}");
            write(&name, &channel);
        }
        Commands::Withdraw { name } => {
            let Some(mut channel) = read(&name) else {
                eprintln!("unable to load channel data");
                return Ok(());
            };
            let request = channel.request_full_withdraw(provider).await?;
            println!("Send this to be signed by the counterparty:\n{request}");
            write(&name, &channel);
        }
        Commands::Receive { name } => {
            let Some(mut channel) = read(&name) else {
                eprintln!("unable to load channel data");
                return Ok(());
            };
            println!("Please paste message:");
            let userop = serde_json::from_str(&read_line())?;
            let request = channel.receive_message(userop, provider.clone()).await?;
            println!("Sign? (y/N)");
            let mut line = read_line();
            line.make_ascii_lowercase();
            if line == "y" {
                let response = channel.sign_message(request, provider).await?;
                println!("Please send this response back:\n{response}");
                write(&name, &channel);
            } else {
                println!("Abort.")
            }
        }
        Commands::Response { name } => todo!(),
        Commands::Cancel { name } => {
            let Some(mut channel) = read(&name) else {
                eprintln!("unable to load channel data");
                return Ok(());
            };
            if channel.cancel_pending_message() {
                write(&name, &channel);
                println!("Cancelled.");
            } else {;
                println!("Nothing to cancel.");
            }
        }
    }
    Ok(())
}

fn read(name: &str) -> Option<Channel> {
    let mut file = dirs::home_dir().unwrap();
    file.push(".ch4nn337");
    file.push(format!("{name}.json"));
    serde_json::from_reader(File::open(file).ok()?).ok()?
}

fn write(name: &str, channel: &Channel) {
    let mut file = dirs::home_dir().unwrap();
    file.push(".ch4nn337");
    file.push(format!("{name}.json"));
    serde_json::to_writer(File::create(file).unwrap(), channel).unwrap();
}

fn read_line() -> String {
    let mut line = String::new();
    stdin().lock().read_line(&mut line).unwrap();
    line.truncate(line.len() - 1);
    line
}