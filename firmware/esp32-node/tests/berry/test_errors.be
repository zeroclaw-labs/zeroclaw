# Error Handling Tests

# Test 11: Invalid GPIO pin
try
  gpio.digitalWrite(99, gpio.HIGH)
  print("test_error_invalid_pin: FAIL")
catch e
  print("test_error_invalid_pin: PASS")
end

# Test 12: Missing script
try
  script.execute("nonexistent_script")
  print("test_error_missing_script: FAIL")
catch e
  print("test_error_missing_script: PASS")
end
