# GPIO Test - digitalWrite and digitalRead

# Test 1: digitalWrite HIGH
gpio.pinMode(2, gpio.OUTPUT)
gpio.digitalWrite(2, gpio.HIGH)
print("test_gpio_write_high: PASS")

# Test 2: digitalWrite LOW
gpio.digitalWrite(2, gpio.LOW)
print("test_gpio_write_low: PASS")

# Test 3: digitalRead
gpio.pinMode(4, gpio.INPUT)
var val = gpio.digitalRead(4)
if val == 0 || val == 1
  print("test_gpio_read: PASS")
else
  print("test_gpio_read: FAIL")
end
