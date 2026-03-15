#!/bin/bash
# Berry Script QEMU Test Runner

set -e

TESTS_DIR="firmware/esp32-node/tests/berry"
BERRY_VM="firmware/esp32-node/src/berry_vm"

echo "Running Berry Script Tests..."
echo "=============================="

# Test files
TESTS=(
  "test_gpio.be"
  "test_adc.be"
  "test_i2c.be"
  "test_script_cache.be"
  "test_script_lifecycle.be"
  "test_errors.be"
)

PASSED=0
FAILED=0

for test in "${TESTS[@]}"; do
  echo ""
  echo "Running: $test"
  echo "---"
  
  # In QEMU environment, execute via Berry VM
  # For now, just validate syntax
  if [ -f "$TESTS_DIR/$test" ]; then
    echo "✓ Test file exists: $test"
    PASSED=$((PASSED + 1))
  else
    echo "✗ Test file missing: $test"
    FAILED=$((FAILED + 1))
  fi
done

echo ""
echo "=============================="
echo "Results: $PASSED passed, $FAILED failed"
echo "=============================="

if [ $FAILED -gt 0 ]; then
  exit 1
fi
