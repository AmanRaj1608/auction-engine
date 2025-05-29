#!/bin/bash

echo "Building the auction engine..."
cargo build --workspace --release

if [ $? -ne 0 ]; then
    echo "Build failed! Please fix compilation errors."
    exit 1
fi

echo ""
echo "Starting the auction engine demo..."
echo "This will run a sequencer and two solvers for 60 seconds."
echo ""

echo "Starting sequencer with 2-second tick..."
./target/release/sequencer --tick 2 &
SEQUENCER_PID=$!

sleep 2

echo "Starting solver s1..."
./target/release/solver --id s1 &
SOLVER1_PID=$!

echo "Starting solver s2..."
./target/release/solver --id s2 &
SOLVER2_PID=$!

echo ""
echo "Auction system is running. Press Ctrl+C to stop..."
echo ""

cleanup() {
    echo ""
    echo "Shutting down auction system..."
    kill -TERM $SOLVER1_PID $SOLVER2_PID $SEQUENCER_PID 2>/dev/null
    sleep 1
    kill -KILL $SOLVER1_PID $SOLVER2_PID $SEQUENCER_PID 2>/dev/null
    echo "Demo stopped."
    exit 0
}

trap cleanup INT TERM

while true; do
    sleep 1
done 