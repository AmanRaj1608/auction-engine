use clap::Parser;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::TcpStream;

use runtime::tcp_utils;
use runtime::types::{
    current_timestamp_secs, Bid, SequencerToSolverMessage, SolverId, SolverToSequencerMessage,
    DEFAULT_BID_SUBMISSION_PORT, DEFAULT_FULFILLMENT_PORT, DEFAULT_ORDER_BROADCAST_PORT,
    DEFAULT_SEQUENCER_HOST,
};

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    id: SolverId,
    #[clap(long, default_value = DEFAULT_SEQUENCER_HOST)]
    sequencer_host: String,
    #[clap(long, default_value_t = DEFAULT_ORDER_BROADCAST_PORT)]
    order_port: u16,
    #[clap(long, default_value_t = DEFAULT_BID_SUBMISSION_PORT)]
    bid_port: u16,
    #[clap(long, default_value_t = DEFAULT_FULFILLMENT_PORT)]
    fulfill_port: u16,
}

async fn submit_bid(args: &Args, bid: Bid) -> Result<(), Box<dyn std::error::Error>> {
    match TcpStream::connect(format!("{}:{}", args.sequencer_host, args.bid_port)).await {
        Ok(stream) => {
            let (_reader, mut writer) = stream.into_split();
            tcp_utils::send_json_message(
                &mut writer,
                &SolverToSequencerMessage::SubmitBid(bid.clone()),
            )
            .await?;
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "[Solver {}] Failed to connect to bid submission port: {e}",
                args.id
            );
            Err(e.into())
        }
    }
}

async fn send_fulfillment_claim(
    args: &Args,
    order_id: u64,
    solver_id: SolverId,
) -> Result<(), Box<dyn std::error::Error>> {
    match TcpStream::connect(format!("{}:{}", args.sequencer_host, args.fulfill_port)).await {
        Ok(stream) => {
            let (_reader, mut writer) = stream.into_split();
            tcp_utils::send_json_message(
                &mut writer,
                &SolverToSequencerMessage::FulfillmentComplete {
                    order_id,
                    solver_id: solver_id.clone(),
                },
            )
            .await?;
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "[Solver {}] Failed to connect to fulfillment port: {e}",
                args.id
            );
            Err(e.into())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("[Solver {}] Starting...", args.id);

    let order_stream_addr = format!("{}:{}", args.sequencer_host, args.order_port);
    let solver_id_clone = args.id.clone();
    let args_clone = args.clone();

    // task to connect to sequencer's order broadcast and listen for messages
    tokio::spawn(async move {
        loop {
            println!(
                "[Solver {solver_id_clone}] Attempting to connect to sequencer at {order_stream_addr} for orders/commitments..."
            );
            match TcpStream::connect(order_stream_addr.clone()).await {
                Ok(stream) => {
                    println!(
                        "[Solver {solver_id_clone}] Connected to sequencer for orders/commitments."
                    );
                    let (reader, _writer) = stream.into_split(); // _writer not used by solver on this channel
                    let mut buf_reader = BufReader::new(reader);

                    loop {
                        match tcp_utils::read_json_message::<SequencerToSolverMessage>(
                            &mut buf_reader,
                        )
                        .await
                        .map_err(|e| format!("Read error: {e}"))
                        {
                            Ok(Some(SequencerToSolverMessage::NewOrderBatch(batch))) => {
                                println!(
                                    "[Solver {solver_id_clone}] Received new order batch with {} orders.",
                                    batch.len()
                                );
                                for order in batch {
                                    let mut rng = SmallRng::seed_from_u64(order.id);
                                    if rng.random_bool(0.7) {
                                        let max_input = rng.random_range(50..200);
                                        let conversion_rate = rng.random_range(0.8..1.2);
                                        let bid_duration_secs = rng.random_range(15..60);

                                        let bid = Bid {
                                            order_id: order.id,
                                            solver_id: solver_id_clone.clone(),
                                            max_input,
                                            conversion_rate,
                                            valid_till: current_timestamp_secs()
                                                + bid_duration_secs,
                                        };
                                        println!(
                                            "[Solver {solver_id_clone}] Decided to bid on order {}: min_output {}, valid_till {}",
                                            order.id,
                                            bid.min_output(),
                                            bid.valid_till
                                        );
                                        if let Err(e) = submit_bid(&args_clone, bid).await {
                                            eprintln!(
                                                "[Solver {solver_id_clone}] Error submitting bid: {e}"
                                            );
                                        }
                                    } else {
                                        println!(
                                            "[Solver {solver_id_clone}] Decided NOT to bid on order {}.",
                                            order.id
                                        );
                                    }
                                }
                            }
                            Ok(Some(SequencerToSolverMessage::CommitmentNotification(
                                commitment,
                            ))) => {
                                if commitment.solver_id == solver_id_clone {
                                    println!(
                                        "[Solver {solver_id_clone}] Received commitment for order {}: {:?}",
                                        commitment.order_id, commitment
                                    );
                                    // simulate work
                                    let work_duration_secs =
                                        SmallRng::seed_from_u64(commitment.order_id).random_range(
                                            1..(commitment.deadline - current_timestamp_secs())
                                                .max(2)
                                                - 1,
                                        );
                                    println!(
                                        "[Solver {solver_id_clone}] Working on order {} for {work_duration_secs} seconds...",
                                        commitment.order_id
                                    );
                                    tokio::time::sleep(Duration::from_secs(work_duration_secs))
                                        .await;

                                    if current_timestamp_secs() < commitment.deadline {
                                        println!(
                                            "[Solver {solver_id_clone}] Work complete for order {}. Sending fulfillment claim.",
                                            commitment.order_id
                                        );
                                        if let Err(e) = send_fulfillment_claim(
                                            &args_clone,
                                            commitment.order_id,
                                            solver_id_clone.clone(),
                                        )
                                        .await
                                        {
                                            eprintln!(
                                                "[Solver {solver_id_clone}] Error sending fulfillment claim: {e}"
                                            );
                                        }
                                    } else {
                                        println!(
                                            "[Solver {solver_id_clone}] MISSED DEADLINE for order {} after work. Deadline: {}, Current: {}",
                                            commitment.order_id,
                                            commitment.deadline,
                                            current_timestamp_secs()
                                        );
                                    }
                                }
                                // else: commitment for another solver, ignore.
                            }
                            Ok(Some(SequencerToSolverMessage::AuctionClosedNoWinner(order_id))) => {
                                println!(
                                    "[Solver {solver_id_clone}] Received auction closed (no winner) for order {order_id}."
                                );
                            }
                            Ok(None) => {
                                // connection closed by sequencer
                                eprintln!(
                                    "[Solver {solver_id_clone}] Sequencer closed the order/commitment connection."
                                );
                                break;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[Solver {solver_id_clone}] Error reading message from sequencer: {e}"
                                );
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[Solver {solver_id_clone}] Failed to connect to sequencer at {order_stream_addr}: {e}. Retrying in 5s..."
                    );
                }
            }
            // wait before retrying connection
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    println!(
        "[Solver {}] Initialized. Will connect to sequencer at {}:{}/{}/{}",
        args.id, args.sequencer_host, args.order_port, args.bid_port, args.fulfill_port
    );
    // keep main alive or handle graceful shutdown
    tokio::signal::ctrl_c().await?;
    println!("[Solver {}] Shutting down...", args.id);
    Ok(())
}
