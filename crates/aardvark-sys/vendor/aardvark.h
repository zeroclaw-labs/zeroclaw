/*=========================================================================
| Aardvark Interface Library
|--------------------------------------------------------------------------
| Copyright (c) 2003-2024 Total Phase, Inc.
| All rights reserved.
| www.totalphase.com
|
| Redistribution and use of this file in source and binary forms, with
| or without modification, are permitted provided that the following
| conditions are met:
|
| - Redistributions of source code must retain the above copyright
|   notice, this list of conditions, and the following disclaimer.
|
| - Redistributions in binary form must reproduce the above copyright
|   notice, this list of conditions, and the following disclaimer in the
|   documentation or other materials provided with the distribution.
|
| - This file must only be used to interface with Total Phase products.
|   The names of Total Phase and its contributors must not be used to
|   endorse or promote products derived from this software.
|
| THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
| "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING BUT NOT
| LIMITED TO THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS
| FOR A PARTICULAR PURPOSE, ARE DISCLAIMED.  IN NO EVENT WILL THE
| COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
| INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING
| BUT NOT LIMITED TO PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
| LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
| CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
| LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN
| ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
| POSSIBILITY OF SUCH DAMAGE.
|--------------------------------------------------------------------------
| To access Total Phase Aardvark devices through the API:
|
| 1) Use one of the following shared objects:
|      aardvark.so       --  Linux or macOS shared object
|      aardvark.dll      --  Windows dynamic link library
|
| 2) Along with one of the following language modules:
|      aardvark.c/h      --  C/C++ API header file and interface module
|      aardvark_py.py    --  Python API
|      aardvark.cs       --  C# .NET source
|      aardvark_net.dll  --  Compiled .NET binding
|      aardvark.bas      --  Visual Basic 6 API
 ========================================================================*/


#ifndef __aardvark_h__
#define __aardvark_h__

#ifdef __cplusplus
extern "C" {
#endif


/*=========================================================================
| TYPEDEFS
 ========================================================================*/
#ifndef TOTALPHASE_DATA_TYPES
#define TOTALPHASE_DATA_TYPES

#ifndef _MSC_VER
/* C99-compliant compilers (GCC) */
#include <stdint.h>
typedef uint8_t   u08;
typedef uint16_t  u16;
typedef uint32_t  u32;
typedef uint64_t  u64;
typedef int8_t    s08;
typedef int16_t   s16;
typedef int32_t   s32;
typedef int64_t   s64;

#else
/* Microsoft compilers (Visual C++) */
typedef unsigned __int8   u08;
typedef unsigned __int16  u16;
typedef unsigned __int32  u32;
typedef unsigned __int64  u64;
typedef signed   __int8   s08;
typedef signed   __int16  s16;
typedef signed   __int32  s32;
typedef signed   __int64  s64;

#endif /* __MSC_VER */

typedef float   f32;
typedef double  f64;

#endif /* TOTALPHASE_DATA_TYPES */


/*=========================================================================
| DEBUG
 ========================================================================*/
/* Set the following macro to '1' for debugging */
#define AA_DEBUG 0


/*=========================================================================
| VERSION
 ========================================================================*/
#define AA_HEADER_VERSION  0x0600   /* v6.00 */


/*=========================================================================
| STATUS CODES
 ========================================================================*/
/*
 * All API functions return an integer which is the result of the
 * transaction, or a status code if negative.  The status codes are
 * defined as follows:
 */
enum AardvarkStatus {
    /* General codes (0 to -99) */
    AA_OK                       =    0,
    AA_UNABLE_TO_LOAD_LIBRARY   =   -1,
    AA_UNABLE_TO_LOAD_DRIVER    =   -2,
    AA_UNABLE_TO_LOAD_FUNCTION  =   -3,
    AA_INCOMPATIBLE_LIBRARY     =   -4,
    AA_INCOMPATIBLE_DEVICE      =   -5,
    AA_COMMUNICATION_ERROR      =   -6,
    AA_UNABLE_TO_OPEN           =   -7,
    AA_UNABLE_TO_CLOSE          =   -8,
    AA_INVALID_HANDLE           =   -9,
    AA_CONFIG_ERROR             =  -10,

    /* I2C codes (-100 to -199) */
    AA_I2C_NOT_AVAILABLE        = -100,
    AA_I2C_NOT_ENABLED          = -101,
    AA_I2C_READ_ERROR           = -102,
    AA_I2C_WRITE_ERROR          = -103,
    AA_I2C_SLAVE_BAD_CONFIG     = -104,
    AA_I2C_SLAVE_READ_ERROR     = -105,
    AA_I2C_SLAVE_TIMEOUT        = -106,
    AA_I2C_DROPPED_EXCESS_BYTES = -107,
    AA_I2C_BUS_ALREADY_FREE     = -108,

    /* SPI codes (-200 to -299) */
    AA_SPI_NOT_AVAILABLE        = -200,
    AA_SPI_NOT_ENABLED          = -201,
    AA_SPI_WRITE_ERROR          = -202,
    AA_SPI_SLAVE_READ_ERROR     = -203,
    AA_SPI_SLAVE_TIMEOUT        = -204,
    AA_SPI_DROPPED_EXCESS_BYTES = -205,

    /* GPIO codes (-400 to -499) */
    AA_GPIO_NOT_AVAILABLE       = -400
};
#ifndef __cplusplus
typedef enum AardvarkStatus AardvarkStatus;
#endif


/*=========================================================================
| GENERAL TYPE DEFINITIONS
 ========================================================================*/
/* Aardvark handle type definition */
typedef int Aardvark;

/*
 * Deprecated type definitions.
 *
 * These are only for use with legacy code and
 * should not be used for new development.
 */
typedef u08 aa_u08;

typedef u16 aa_u16;

typedef u32 aa_u32;

typedef s08 aa_s08;

typedef s16 aa_s16;

typedef s32 aa_s32;

/*
 * Aardvark version matrix.
 *
 * This matrix describes the various version dependencies
 * of Aardvark components.  It can be used to determine
 * which component caused an incompatibility error.
 *
 * All version numbers are of the format:
 *   (major << 8) | minor
 *
 * ex. v1.20 would be encoded as:  0x0114
 */
struct AardvarkVersion {
    /* Software, firmware, and hardware versions. */
    u16 software;
    u16 firmware;
    u16 hardware;

    /* Firmware requires that software must be >= this version. */
    u16 sw_req_by_fw;

    /* Software requires that firmware must be >= this version. */
    u16 fw_req_by_sw;

    /* Software requires that the API interface must be >= this version. */
    u16 api_req_by_sw;
};
#ifndef __cplusplus
typedef struct AardvarkVersion AardvarkVersion;
#endif


/*=========================================================================
| GENERAL API
 ========================================================================*/
/*
 * Get a list of ports to which Aardvark devices are attached.
 *
 * nelem   = maximum number of elements to return
 * devices = array into which the port numbers are returned
 *
 * Each element of the array is written with the port number.
 * Devices that are in-use are ORed with AA_PORT_NOT_FREE (0x8000).
 *
 * ex.  devices are attached to ports 0, 1, 2
 *      ports 0 and 2 are available, and port 1 is in-use.
 *      array => 0x0000, 0x8001, 0x0002
 *
 * If the array is NULL, it is not filled with any values.
 * If there are more devices than the array size, only the
 * first nmemb port numbers will be written into the array.
 *
 * Returns the number of devices found, regardless of the
 * array size.
 */
#define AA_PORT_NOT_FREE 0x8000
int aa_find_devices (
    int   num_devices,
    u16 * devices
);


/*
 * Get a list of ports to which Aardvark devices are attached.
 *
 * This function is the same as aa_find_devices() except that
 * it returns the unique IDs of each Aardvark device.  The IDs
 * are guaranteed to be non-zero if valid.
 *
 * The IDs are the unsigned integer representation of the 10-digit
 * serial numbers.
 */
int aa_find_devices_ext (
    int   num_devices,
    u16 * devices,
    int   num_ids,
    u32 * unique_ids
);


/*
 * Open the Aardvark port.
 *
 * The port number is a zero-indexed integer.
 *
 * The port number is the same as that obtained from the
 * aa_find_devices() function above.
 *
 * Returns an Aardvark handle, which is guaranteed to be
 * greater than zero if it is valid.
 *
 * This function is recommended for use in simple applications
 * where extended information is not required.  For more complex
 * applications, the use of aa_open_ext() is recommended.
 */
Aardvark aa_open (
    int port_number
);


/*
 * Open the Aardvark port, returning extended information
 * in the supplied structure.  Behavior is otherwise identical
 * to aa_open() above.  If 0 is passed as the pointer to the
 * structure, this function is exactly equivalent to aa_open().
 *
 * The structure is zeroed before the open is attempted.
 * It is filled with whatever information is available.
 *
 * For example, if the firmware version is not filled, then
 * the device could not be queried for its version number.
 *
 * This function is recommended for use in complex applications
 * where extended information is required.  For more simple
 * applications, the use of aa_open() is recommended.
 */
struct AardvarkExt {
    /* Version matrix */
    AardvarkVersion   version;

    /* Features of this device. */
    int               features;
};
#ifndef __cplusplus
typedef struct AardvarkExt AardvarkExt;
#endif

Aardvark aa_open_ext (
    int           port_number,
    AardvarkExt * aa_ext
);


/* Close the Aardvark port. */
int aa_close (
    Aardvark aardvark
);


/*
 * Return the port for this Aardvark handle.
 *
 * The port number is a zero-indexed integer.
 */
int aa_port (
    Aardvark aardvark
);


/*
 * Return the device features as a bit-mask of values, or
 * an error code if the handle is not valid.
 */
#define AA_FEATURE_SPI 0x00000001
#define AA_FEATURE_I2C 0x00000002
#define AA_FEATURE_GPIO 0x00000008
int aa_features (
    Aardvark aardvark
);


/*
 * Return the unique ID for this Aardvark adapter.
 * IDs are guaranteed to be non-zero if valid.
 * The ID is the unsigned integer representation of the
 * 10-digit serial number.
 */
u32 aa_unique_id (
    Aardvark aardvark
);


/*
 * Return the status string for the given status code.
 * If the code is not valid or the library function cannot
 * be loaded, return a NULL string.
 */
const char * aa_status_string (
    int status
);


/*
 * Enable logging to a file.  The handle must be standard file
 * descriptor.  In C, a file descriptor can be obtained by using
 * the ANSI C function "open" or by using the function "fileno"
 * on a FILE* stream.  A FILE* stream can be obtained using "fopen"
 * or can correspond to the common "stdout" or "stderr" --
 * available when including stdlib.h
 */
#define AA_LOG_STDOUT 1
#define AA_LOG_STDERR 2
int aa_log (
    Aardvark aardvark,
    int      level,
    int      handle
);


/*
 * Return the version matrix for the device attached to the
 * given handle.  If the handle is 0 or invalid, only the
 * software and required api versions are set.
 */
int aa_version (
    Aardvark          aardvark,
    AardvarkVersion * version
);


/*
 * Configure the device by enabling/disabling I2C, SPI, and
 * GPIO functions.
 */
enum AardvarkConfig {
    AA_CONFIG_GPIO_ONLY = 0x00,
    AA_CONFIG_SPI_GPIO  = 0x01,
    AA_CONFIG_GPIO_I2C  = 0x02,
    AA_CONFIG_SPI_I2C   = 0x03,
    AA_CONFIG_QUERY     = 0x80
};
#ifndef __cplusplus
typedef enum AardvarkConfig AardvarkConfig;
#endif

#define AA_CONFIG_SPI_MASK 0x00000001
#define AA_CONFIG_I2C_MASK 0x00000002
int aa_configure (
    Aardvark       aardvark,
    AardvarkConfig config
);


/*
 * Configure the target power pins.
 * This is only supported on hardware versions >= 2.00
 */
#define AA_TARGET_POWER_NONE 0x00
#define AA_TARGET_POWER_BOTH 0x03
#define AA_TARGET_POWER_QUERY 0x80
int aa_target_power (
    Aardvark aardvark,
    u08      power_mask
);


/*
 * Sleep for the specified number of milliseconds
 * Accuracy depends on the operating system scheduler
 * Returns the number of milliseconds slept
 */
u32 aa_sleep_ms (
    u32 milliseconds
);



/*=========================================================================
| ASYNC MESSAGE POLLING
 ========================================================================*/
/*
 * Polling function to check if there are any asynchronous
 * messages pending for processing. The function takes a timeout
 * value in units of milliseconds.  If the timeout is < 0, the
 * function will block until data is received.  If the timeout is 0,
 * the function will perform a non-blocking check.
 */
#define AA_ASYNC_NO_DATA 0x00000000
#define AA_ASYNC_I2C_READ 0x00000001
#define AA_ASYNC_I2C_WRITE 0x00000002
#define AA_ASYNC_SPI 0x00000004
int aa_async_poll (
    Aardvark aardvark,
    int      timeout
);



/*=========================================================================
| I2C API
 ========================================================================*/
/* Free the I2C bus. */
int aa_i2c_free_bus (
    Aardvark aardvark
);


/*
 * Set the I2C bit rate in kilohertz.  If a zero is passed as the
 * bitrate, the bitrate is unchanged and the current bitrate is
 * returned.
 */
int aa_i2c_bitrate (
    Aardvark aardvark,
    int      bitrate_khz
);


/*
 * Set the bus lock timeout.  If a zero is passed as the timeout,
 * the timeout is unchanged and the current timeout is returned.
 */
int aa_i2c_bus_timeout (
    Aardvark aardvark,
    u16      timeout_ms
);


enum AardvarkI2cFlags {
    AA_I2C_NO_FLAGS          = 0x00,
    AA_I2C_10_BIT_ADDR       = 0x01,
    AA_I2C_COMBINED_FMT      = 0x02,
    AA_I2C_NO_STOP           = 0x04,
    AA_I2C_SIZED_READ        = 0x10,
    AA_I2C_SIZED_READ_EXTRA1 = 0x20
};
#ifndef __cplusplus
typedef enum AardvarkI2cFlags AardvarkI2cFlags;
#endif

/* Read a stream of bytes from the I2C slave device. */
int aa_i2c_read (
    Aardvark         aardvark,
    u16              slave_addr,
    AardvarkI2cFlags flags,
    u16              num_bytes,
    u08 *            data_in
);


enum AardvarkI2cStatus {
    AA_I2C_STATUS_OK            = 0,
    AA_I2C_STATUS_BUS_ERROR     = 1,
    AA_I2C_STATUS_SLA_ACK       = 2,
    AA_I2C_STATUS_SLA_NACK      = 3,
    AA_I2C_STATUS_DATA_NACK     = 4,
    AA_I2C_STATUS_ARB_LOST      = 5,
    AA_I2C_STATUS_BUS_LOCKED    = 6,
    AA_I2C_STATUS_LAST_DATA_ACK = 7
};
#ifndef __cplusplus
typedef enum AardvarkI2cStatus AardvarkI2cStatus;
#endif

/*
 * Read a stream of bytes from the I2C slave device.
 * This API function returns the number of bytes read into
 * the num_read variable.  The return value of the function
 * is a status code.
 */
int aa_i2c_read_ext (
    Aardvark         aardvark,
    u16              slave_addr,
    AardvarkI2cFlags flags,
    u16              num_bytes,
    u08 *            data_in,
    u16 *            num_read
);


/* Write a stream of bytes to the I2C slave device. */
int aa_i2c_write (
    Aardvark         aardvark,
    u16              slave_addr,
    AardvarkI2cFlags flags,
    u16              num_bytes,
    const u08 *      data_out
);


/*
 * Write a stream of bytes to the I2C slave device.
 * This API function returns the number of bytes written into
 * the num_written variable.  The return value of the function
 * is a status code.
 */
int aa_i2c_write_ext (
    Aardvark         aardvark,
    u16              slave_addr,
    AardvarkI2cFlags flags,
    u16              num_bytes,
    const u08 *      data_out,
    u16 *            num_written
);


/*
 * Do an atomic write+read to an I2C slave device by first
 * writing a stream of bytes to the I2C slave device and then
 * reading a stream of bytes back from the same slave device.
 * This API function returns the number of bytes written into
 * the num_written variable and the number of bytes read into
 * the num_read variable.  The return value of the function is
 * the status given as (read_status << 8) | (write_status).
 */
int aa_i2c_write_read (
    Aardvark         aardvark,
    u16              slave_addr,
    AardvarkI2cFlags flags,
    u16              out_num_bytes,
    const u08 *      out_data,
    u16 *            num_written,
    u16              in_num_bytes,
    u08 *            in_data,
    u16 *            num_read
);


/* Enable/Disable the Aardvark as an I2C slave device */
int aa_i2c_slave_enable (
    Aardvark aardvark,
    u08      addr,
    u16      maxTxBytes,
    u16      maxRxBytes
);


int aa_i2c_slave_disable (
    Aardvark aardvark
);


/*
 * Set the slave response in the event the Aardvark is put
 * into slave mode and contacted by a Master.
 */
int aa_i2c_slave_set_response (
    Aardvark    aardvark,
    u08         num_bytes,
    const u08 * data_out
);


/*
 * Return number of bytes written from a previous
 * Aardvark->I2C_master transmission.  Since the transmission is
 * happening asynchronously with respect to the PC host
 * software, there could be responses queued up from many
 * previous write transactions.
 */
int aa_i2c_slave_write_stats (
    Aardvark aardvark
);


/* Read the bytes from an I2C slave reception */
int aa_i2c_slave_read (
    Aardvark aardvark,
    u08 *    addr,
    u16      num_bytes,
    u08 *    data_in
);


/* Extended functions that return status code */
int aa_i2c_slave_write_stats_ext (
    Aardvark aardvark,
    u16 *    num_written
);


int aa_i2c_slave_read_ext (
    Aardvark aardvark,
    u08 *    addr,
    u16      num_bytes,
    u08 *    data_in,
    u16 *    num_read
);


/*
 * Configure the I2C pullup resistors.
 * This is only supported on hardware versions >= 2.00
 */
#define AA_I2C_PULLUP_NONE 0x00
#define AA_I2C_PULLUP_BOTH 0x03
#define AA_I2C_PULLUP_QUERY 0x80
int aa_i2c_pullup (
    Aardvark aardvark,
    u08      pullup_mask
);



/*=========================================================================
| SPI API
 ========================================================================*/
/*
 * Set the SPI bit rate in kilohertz.  If a zero is passed as the
 * bitrate, the bitrate is unchanged and the current bitrate is
 * returned.
 */
int aa_spi_bitrate (
    Aardvark aardvark,
    int      bitrate_khz
);


/*
 * These configuration parameters specify how to clock the
 * bits that are sent and received on the Aardvark SPI
 * interface.
 *
 *   The polarity option specifies which transition
 *   constitutes the leading edge and which transition is the
 *   falling edge.  For example, AA_SPI_POL_RISING_FALLING
 *   would configure the SPI to idle the SCK clock line low.
 *   The clock would then transition low-to-high on the
 *   leading edge and high-to-low on the trailing edge.
 *
 *   The phase option determines whether to sample or setup on
 *   the leading edge.  For example, AA_SPI_PHASE_SAMPLE_SETUP
 *   would configure the SPI to sample on the leading edge and
 *   setup on the trailing edge.
 *
 *   The bitorder option is used to indicate whether LSB or
 *   MSB is shifted first.
 *
 * See the diagrams in the Aardvark datasheet for
 * more details.
 */
enum AardvarkSpiPolarity {
    AA_SPI_POL_RISING_FALLING = 0,
    AA_SPI_POL_FALLING_RISING = 1
};
#ifndef __cplusplus
typedef enum AardvarkSpiPolarity AardvarkSpiPolarity;
#endif

enum AardvarkSpiPhase {
    AA_SPI_PHASE_SAMPLE_SETUP = 0,
    AA_SPI_PHASE_SETUP_SAMPLE = 1
};
#ifndef __cplusplus
typedef enum AardvarkSpiPhase AardvarkSpiPhase;
#endif

enum AardvarkSpiBitorder {
    AA_SPI_BITORDER_MSB = 0,
    AA_SPI_BITORDER_LSB = 1
};
#ifndef __cplusplus
typedef enum AardvarkSpiBitorder AardvarkSpiBitorder;
#endif

/* Configure the SPI master or slave interface */
int aa_spi_configure (
    Aardvark            aardvark,
    AardvarkSpiPolarity polarity,
    AardvarkSpiPhase    phase,
    AardvarkSpiBitorder bitorder
);


/* Write a stream of bytes to the downstream SPI slave device. */
int aa_spi_write (
    Aardvark    aardvark,
    u16         out_num_bytes,
    const u08 * data_out,
    u16         in_num_bytes,
    u08 *       data_in
);


/* Enable/Disable the Aardvark as an SPI slave device */
int aa_spi_slave_enable (
    Aardvark aardvark
);


int aa_spi_slave_disable (
    Aardvark aardvark
);


/*
 * Set the slave response in the event the Aardvark is put
 * into slave mode and contacted by a Master.
 */
int aa_spi_slave_set_response (
    Aardvark    aardvark,
    u08         num_bytes,
    const u08 * data_out
);


/* Read the bytes from an SPI slave reception */
int aa_spi_slave_read (
    Aardvark aardvark,
    u16      num_bytes,
    u08 *    data_in
);


/*
 * Change the output polarity on the SS line.
 *
 * Note: When configured as an SPI slave, the Aardvark will
 * always be setup with SS as active low.  Hence this function
 * only affects the SPI master functions on the Aardvark.
 */
enum AardvarkSpiSSPolarity {
    AA_SPI_SS_ACTIVE_LOW  = 0,
    AA_SPI_SS_ACTIVE_HIGH = 1
};
#ifndef __cplusplus
typedef enum AardvarkSpiSSPolarity AardvarkSpiSSPolarity;
#endif

int aa_spi_master_ss_polarity (
    Aardvark              aardvark,
    AardvarkSpiSSPolarity polarity
);



/*=========================================================================
| GPIO API
 ========================================================================*/
/*
 * The following enumerated type maps the named lines on the
 * Aardvark I2C/SPI line to bit positions in the GPIO API.
 * All GPIO API functions will index these lines through an
 * 8-bit masked value.  Thus, each bit position in the mask
 * can be referred back its corresponding line through the
 * enumerated type.
 */
enum AardvarkGpioBits {
    AA_GPIO_SCL  = 0x01,
    AA_GPIO_SDA  = 0x02,
    AA_GPIO_MISO = 0x04,
    AA_GPIO_SCK  = 0x08,
    AA_GPIO_MOSI = 0x10,
    AA_GPIO_SS   = 0x20
};
#ifndef __cplusplus
typedef enum AardvarkGpioBits AardvarkGpioBits;
#endif

/*
 * Configure the GPIO, specifying the direction of each bit.
 *
 * A call to this function will not change the value of the pullup
 * mask in the Aardvark.  This is illustrated by the following
 * example:
 *   (1) Direction mask is first set to 0x00
 *   (2) Pullup is set to 0x01
 *   (3) Direction mask is set to 0x01
 *   (4) Direction mask is later set back to 0x00.
 *
 * The pullup will be active after (4).
 *
 * On Aardvark power-up, the default value of the direction
 * mask is 0x00.
 */
#define AA_GPIO_DIR_INPUT 0
#define AA_GPIO_DIR_OUTPUT 1
int aa_gpio_direction (
    Aardvark aardvark,
    u08      direction_mask
);


/*
 * Enable an internal pullup on any of the GPIO input lines.
 *
 * Note: If a line is configured as an output, the pullup bit
 * for that line will be ignored, though that pullup bit will
 * be cached in case the line is later configured as an input.
 *
 * By default the pullup mask is 0x00.
 */
#define AA_GPIO_PULLUP_OFF 0
#define AA_GPIO_PULLUP_ON 1
int aa_gpio_pullup (
    Aardvark aardvark,
    u08      pullup_mask
);


/*
 * Read the current digital values on the GPIO input lines.
 *
 * The bits will be ordered as described by AA_GPIO_BITS.  If a
 * line is configured as an output, its corresponding bit
 * position in the mask will be undefined.
 */
int aa_gpio_get (
    Aardvark aardvark
);


/*
 * Set the outputs on the GPIO lines.
 *
 * Note: If a line is configured as an input, it will not be
 * affected by this call, but the output value for that line
 * will be cached in the event that the line is later
 * configured as an output.
 */
int aa_gpio_set (
    Aardvark aardvark,
    u08      value
);


/*
 * Block until there is a change on the GPIO input lines.
 * Pins configured as outputs will be ignored.
 *
 * The function will return either when a change has occurred or
 * the timeout expires.  The timeout, specified in millisecods, has
 * a precision of ~16 ms. The maximum allowable timeout is
 * approximately 4 seconds. If the timeout expires, this function
 * will return the current state of the GPIO lines.
 *
 * This function will return immediately with the current value
 * of the GPIO lines for the first invocation after any of the
 * following functions are called: aa_configure,
 * aa_gpio_direction, or aa_gpio_pullup.
 *
 * If the function aa_gpio_get is called before calling
 * aa_gpio_change, aa_gpio_change will only register any changes
 * from the value last returned by aa_gpio_get.
 */
int aa_gpio_change (
    Aardvark aardvark,
    u16      timeout
);




#ifdef __cplusplus
}
#endif

#endif  /* __aardvark_h__ */
