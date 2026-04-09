#!/bin/bash
set -e

IMAGE_NAME="greetd-test"
CONTAINER_NAME="greetd-test-run"

echo "=== Building Docker image ==="
docker build -t "$IMAGE_NAME" .

echo ""
echo "=== Running unit tests ==="
cargo test --no-default-features 2>&1 || cargo test

echo ""
echo "=== Running integration test in Docker ==="

# Clean up any previous run
docker rm -f "$CONTAINER_NAME" 2>/dev/null || true

# Run container detached
docker run -d --name "$CONTAINER_NAME" "$IMAGE_NAME"

# Wait for greeter to complete its cycle (auth -> start session -> session exits)
sleep 5

# Get logs
LOGS=$(docker logs "$CONTAINER_NAME" 2>&1)

# Stop container
docker stop "$CONTAINER_NAME" >/dev/null 2>&1 || true
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

echo "$LOGS"

# Check for success
if echo "$LOGS" | grep -q "session started"; then
    echo ""
    echo "=== Integration test PASSED ==="
    exit 0
else
    echo ""
    echo "=== Integration test FAILED ==="
    echo "Expected 'session started' in output"
    exit 1
fi
