# ADC Test - analogRead

# Test 4: analogRead on valid pin
var adc_val = gpio.analogRead(34)
if adc_val >= 0 && adc_val <= 4095
  print("test_adc_read: PASS")
else
  print("test_adc_read: FAIL")
end
