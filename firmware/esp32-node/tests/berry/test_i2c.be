# I2C Test - basic I2C operations

# Test 5: I2C begin
i2c.begin(21, 22, 100000)
print("test_i2c_begin: PASS")

# Test 6: I2C scan (simulated)
var devices = i2c.scan()
print("test_i2c_scan: PASS")
