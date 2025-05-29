use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub type OrderId = u64;
pub type SolverId = String;
pub type Amount = u64;
pub type Timestamp = u64;

/// represents an order placed on the system
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Order {
    pub id: OrderId,
    pub description: String,
    pub creation_time: Timestamp,
}

/// represents a bid made by a solver for an order
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bid {
    pub order_id: OrderId,
    pub solver_id: SolverId,
    pub max_input: Amount,
    pub conversion_rate: f64,
    pub valid_till: Timestamp,
}

impl Bid {
    /// calculates the minimum output based on max_input and conversion_rate
    pub fn min_output(&self) -> Amount {
        (self.max_input as f64 * self.conversion_rate).floor() as Amount
    }
}

/// represents a commitment made by the sequencer to a solver for an order
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Commitment {
    pub order_id: OrderId,
    pub solver_id: SolverId,
    pub input_amount: Amount,
    pub min_output_amount: Amount,
    pub deadline: Timestamp,
}

/// messages sent from the sequencer to solvers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SequencerToSolverMessage {
    NewOrderBatch(Vec<Order>),
    CommitmentNotification(Commitment),
    AuctionClosedNoWinner(OrderId),
}

/// messages sent from a solver to the sequencer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SolverToSequencerMessage {
    SubmitBid(Bid),
    FulfillmentComplete {
        order_id: OrderId,
        solver_id: SolverId,
    },
}

/// events logged by the sequencer to its append-only chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChainEvent {
    OrderAdded(Order),
    BatchCreated(Vec<OrderId>),
    BidReceived(Bid),
    BidSelected {
        order_id: OrderId,
        solver_id: SolverId,
        commitment: Commitment,
    },
    OrderFulfilled {
        order_id: OrderId,
        solver_id: SolverId,
    },
    OrderFailedToFulfill {
        order_id: OrderId,
        solver_id: SolverId,
    },
    OrderReassigned {
        order_id: OrderId,
        prev_solver_id: SolverId,
        new_solver_id: SolverId,
        new_commitment: Commitment,
    },
    OrderExpired(OrderId),
}

/// utility to get current unix timestamp in seconds
pub fn current_timestamp_secs() -> Timestamp {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub const GRACE_PERIOD_SECS: u64 = 2;
pub const COMMITMENT_DURATION_SECS: u64 = 10;
pub const ORDER_BATCH_SIZE: usize = 5;
pub const BID_COLLECTION_PERIOD_SECS: u64 = 5;

pub const DEFAULT_SEQUENCER_HOST: &str = "127.0.0.1";
pub const DEFAULT_ORDER_BROADCAST_PORT: u16 = 8080;
pub const DEFAULT_BID_SUBMISSION_PORT: u16 = 8081;
pub const DEFAULT_FULFILLMENT_PORT: u16 = 8082;
