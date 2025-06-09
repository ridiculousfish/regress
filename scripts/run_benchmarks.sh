#!/bin/bash

# Performance testing script for regress
# Usage: ./scripts/run_benchmarks.sh [options]

set -eux -o pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default values
BASELINE=""
OUTPUT_DIR="target/criterion"
SAVE_BASELINE=""
COMPARE_MODE=""

# Function to print usage
usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Run performance benchmarks for the regress crate.

OPTIONS:
    -h, --help          Show this help message
    -b, --baseline NAME Save current results as baseline with given name
    -c, --compare NAME  Compare current results with saved baseline
    -o, --output DIR    Output directory for results (default: target/criterion)
    -v, --verbose       Verbose output
    
EXAMPLES:
    # Run benchmarks and save as baseline
    $0 --baseline main

    # Run benchmarks and compare with previous baseline
    $0 --compare main

    # Just run benchmarks without saving/comparing
    $0
EOF
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -h|--help)
            usage
            exit 0
            ;;
        -b|--baseline)
            SAVE_BASELINE="$2"
            shift 2
            ;;
        -c|--compare)
            COMPARE_MODE="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -v|--verbose)
            VERBOSE=1
            shift
            ;;
        *)
            echo "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

echo -e "${BLUE}=== Regress Performance Benchmarks ===${NC}"
echo "Timestamp: $(date)"
echo "Git commit: $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
echo "Git branch: $(git branch --show-current 2>/dev/null || echo 'unknown')"
echo ""

# Ensure we're in the right directory
if [[ ! -f "Cargo.toml" ]]; then
    echo -e "${RED}Error: Must be run from the project root directory${NC}"
    exit 1
fi

# Check if criterion is available
if ! grep -q "criterion" Cargo.toml; then
    echo -e "${RED}Error: Criterion not found in Cargo.toml. Please add criterion as a dev dependency.${NC}"
    exit 1
fi

# Build in release mode first
echo -e "${YELLOW}Building in release mode...${NC}"
cargo build --release

# Run the benchmarks
echo -e "${YELLOW}Running benchmarks...${NC}"
if [[ -n "$COMPARE_MODE" ]]; then
    echo "Comparing with baseline: $COMPARE_MODE"
    cargo bench -- --baseline "$COMPARE_MODE"
else
    cargo bench
fi

# Save baseline if requested
if [[ -n "$SAVE_BASELINE" ]]; then
    echo -e "${YELLOW}Saving current results as baseline: $SAVE_BASELINE${NC}"
    cp -r "$OUTPUT_DIR" "$OUTPUT_DIR.$SAVE_BASELINE"
    echo -e "${GREEN}Baseline saved successfully${NC}"
fi

# Generate summary report
echo -e "${YELLOW}Generating summary report...${NC}"
REPORT_FILE="benchmark_summary_$(date +%Y%m%d_%H%M%S).txt"

cat > "$REPORT_FILE" << EOF
Regress Performance Benchmark Summary
=====================================
Date: $(date)
Git Commit: $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')
Git Branch: $(git branch --show-current 2>/dev/null || echo 'unknown')
Rust Version: $(rustc --version)

Benchmark Results:
EOF

# If criterion HTML reports are enabled, provide link
if [[ -d "$OUTPUT_DIR" ]]; then
    echo "" >> "$REPORT_FILE"
    echo "Detailed HTML reports available at: $OUTPUT_DIR/report/index.html" >> "$REPORT_FILE"
fi

echo "" >> "$REPORT_FILE"
echo "Run 'cargo bench' to see detailed timing information." >> "$REPORT_FILE"

echo -e "${GREEN}Summary report saved to: $REPORT_FILE${NC}"

# Display quick summary
echo -e "${BLUE}=== Quick Summary ===${NC}"
echo "✓ Benchmarks completed successfully"
echo "✓ Results available in: $OUTPUT_DIR"
if [[ -n "$SAVE_BASELINE" ]]; then
    echo "✓ Baseline saved as: $SAVE_BASELINE"
fi
echo "✓ Summary report: $REPORT_FILE"

echo ""
echo -e "${GREEN}Performance testing complete!${NC}"
echo ""
echo "Next steps:"
echo "- Open $OUTPUT_DIR/report/index.html to view detailed results"
echo "- Compare with previous runs using: $0 --compare <baseline_name>"
echo "- Set up CI to run this script on every commit" 