# Order-Auction Mini-Sim

A minimal working prototype of a three-phase auction engine built in Rust using Tokio for async runtime.

## Overview

This auction engine implements a three-phase order processing system:

1. **Phase 1 - Bid Solicitation**: The sequencer commits batches of orders to an append-only log. Solvers read these batches and submit bids with their maximum input amount, conversion rate, and validity period.

2. **Phase 2 - Bid Selection**: The sequencer evaluates all received bids and selects the best bid for each order based on the highest `min_output` (calculated as `max_input * conversion_rate`). A commitment is created with input amount, minimum output amount, and a deadline.

3. **Phase 3 - Fulfillment**: Selected solvers must deposit the minimum output amount into escrow before the deadline. After the deadline plus a grace period, the sequencer either marks the order as filled or reassigns it to the next best bidder.

## Architecture

The system consists of three main components:

### Runtime Crate

- **Types**: Core data structures (`Order`, `Bid`, `Commitment`, etc.)
- **ChainLog**: In-memory append-only log for recording all events
- **OrderStateManager**: Manages order lifecycle, bid collection, and commitment tracking
- **InMemoryEscrow**: Simplified escrow implementation for tracking fulfillment claims
- **TCP Utils**: Helper functions for JSON message serialization over TCP

### Sequencer Binary

- Manages the entire auction lifecycle
- Runs on a configurable tick interval (default: 2 seconds)
- Listens on three ports:
  - Order broadcast port (8080): Broadcasts new order batches and commitments to connected solvers
  - Bid submission port (8081): Receives bid submissions from solvers
  - Fulfillment port (8082): Receives fulfillment claims from solvers
- Generates synthetic orders periodically
- Handles bid selection, commitment creation, and order reassignment

### Solver Binary

- Connects to the sequencer to receive order batches
- Implements probabilistic bidding logic (70% chance to bid on each order)
- Generates random bid parameters (max_input, conversion_rate, validity period)
- Simulates work when awarded a commitment
- Sends fulfillment claims before deadlines

## Build Instructions

```bash
# Build all components in release mode
cargo build --workspace --release

# Run tests (when available)
cargo test --workspace
```

## Running the System

### Start the Sequencer

```bash
target/release/sequencer --tick 2
```

Options:

- `--tick`: Tick interval in seconds (default: 2)
- `--order-port`: Port for order/commitment broadcasts (default: 8080)
- `--bid-port`: Port for bid submissions (default: 8081)
- `--fulfill-port`: Port for fulfillment claims (default: 8082)

### Start Multiple Solvers

```bash
# Terminal 1
target/release/solver --id s1

# Terminal 2
target/release/solver --id s2

# Terminal 3 (optional)
target/release/solver --id s3
```

Options:

- `--id`: Unique solver identifier (required)
- `--sequencer-host`: Sequencer host address (default: 127.0.0.1)
- `--order-port`: Port to connect for orders (default: 8080)
- `--bid-port`: Port for bid submission (default: 8081)
- `--fulfill-port`: Port for fulfillment claims (default: 8082)

## Design Decisions & Trade-offs

### Network Architecture

- **TCP-based messaging**: Chosen for reliability and ordered message delivery
- **Multiple ports**: Separates concerns and allows for independent scaling of different message types
- **JSON serialization**: Simple and debuggable, though less efficient than binary protocols

### State Management

- **In-memory storage**: Simplifies implementation but limits persistence
- **Mutex-based concurrency**: Simple to reason about, though could be optimized with lock-free structures
- **Arc<Mutex<T>> pattern**: Enables safe sharing across async tasks

### Auction Mechanics

- **Fixed tick intervals**: Provides predictable behavior but may not be optimal for all order types
- **Best bid selection**: Currently based solely on min_output; could incorporate other factors
- **Grace period**: Allows for network delays but increases order completion time

### Solver Strategy

- **Random bidding**: Demonstrates the system but not representative of real solver behavior
- **Synchronous fulfillment**: Solvers process one commitment at a time; could be parallelized

### Error Handling

- **Connection resilience**: Solvers automatically reconnect on connection loss
- **Graceful degradation**: System continues operating even if some solvers disconnect
- **Logging over panic**: Errors are logged rather than crashing the system

## Future Improvements

1. **Persistence**: Add database support for order and bid history
2. **Real escrow**: Implement actual fund locking mechanism
3. **Advanced bidding strategies**: Machine learning-based solver implementations
4. **Metrics & monitoring**: Prometheus metrics for system observability
5. **Security**: Add authentication and encrypted communications
6. **Load balancing**: Distribute sequencer load across multiple instances
7. **Configuration**: YAML/TOML configuration files instead of CLI args
8. **Testing**: Comprehensive unit and integration test suite

## Example Output

### Sequencer

```
[Sequencer] Listening for solvers on port 8080...
[Sequencer] Listening for bids on port 8081...
[Sequencer] Listening for fulfillments on port 8082...

--- Sequencer Tick ---
[Sequencer] Added new order ID: 1
[Sequencer] Created batch with orders: [1]
[ChainEvent] OrderAdded(Order { id: 1, description: "Sample Order #1", creation_time: 1234567890 })
[ChainEvent] BatchCreated([1])

[Sequencer] New solver connected: 127.0.0.1:54321
[Sequencer] Received bid: Bid { order_id: 1, solver_id: "s1", max_input: 150, conversion_rate: 1.1, valid_till: 1234567950 }
[ChainEvent] BidReceived(Bid { ... })

--- Sequencer Tick ---
[Sequencer] Selecting best bid for order 1: Bid { solver_id: "s1", min_output: 165, ... }
[ChainEvent] BidSelected { order_id: 1, solver_id: "s1", commitment: ... }
```

### Solver

```
[Solver s1] Starting...
[Solver s1] Connected to sequencer for orders/commitments.
[Solver s1] Received new order batch with 1 orders.
[Solver s1] Decided to bid on order 1: min_output 165, valid_till 1234567950
[Solver s1] Received commitment for order 1: Commitment { ... }
[Solver s1] Working on order 1 for 3 seconds...
[Solver s1] Work complete for order 1. Sending fulfillment claim.
```
