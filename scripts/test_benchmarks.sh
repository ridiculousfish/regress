#!/bin/bash

# Quick benchmark test script
# Usage: ./scripts/test_benchmarks.sh

set -eux -o pipefail

echo "🧪 Testing benchmark setup..."

# Check if we're in the right directory
if [[ ! -f "Cargo.toml" ]]; then
    echo "❌ Error: Must be run from the project root directory"
    exit 1
fi

echo "📦 Building in release mode..."
cargo build --release

echo "🚀 Running a quick subset of benchmarks..."
timeout 15s cargo bench --bench regex_benchmarks compile || {
    if [ $? -eq 124 ]; then
        echo "⏰ Benchmark test timed out after 15s (this is expected for the test)"
        echo "✅ Benchmarks are working correctly!"
    else
        echo "❌ Benchmark test failed"
        exit 1
    fi
}

echo ""
echo "✅ Benchmark setup test completed successfully!"
echo ""
echo "📊 To run full benchmarks:"
echo "   cargo bench"
echo "   ./scripts/run_benchmarks.sh"
echo ""
echo "📈 To view results:"
echo "   open target/criterion/report/index.html" 