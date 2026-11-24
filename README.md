<h1><img src="https://theweft.org/brand/repos/weft-parts.svg" alt="" width="44" height="44" align="absmiddle"> weft-parts</h1>

> **Status: experimental (0.0.x).** APIs and formats change without notice. Not for production use yet.

Machine-readable chip data and behavioural chip models, part of [weft](https://theweft.org).
One TOML file per chip under [chips/](chips/) serves three jobs:

| Job | Uses | Consumer |
|---|---|---|
| Check PCB wiring | packages, part numbers, pinouts, address straps | [weft-boardchecker](https://github.com/theweft/weft-boardchecker) against IPC-2581 netlists |
| Check device trees | compatible strings, Linux binding and driver, GPIO property to pin | weft-boardchecker; [dt-schema](https://github.com/devicetree-org/dt-schema) validates against the binding itself |
| Simulate the chip | behavioural model behind I2C transfers | Linux drivers ([weft-sim](https://github.com/theweft/weft-sim)), Rust MCU drivers via embedded-hal, unit tests |

"Digital twin" is close; "behavioural model" or "bus-level model" is the usual term in
simulation. The models answer the bus traffic the way the chip does, take the time the chip
takes, and expose analog inputs and outputs to the test. They do not model electrical behaviour.

## Chips

| Chip | Packages | Model covers | Model does not cover |
|---|---|---|---|
| TI ADS1115 ADC | VSSOP-10 | registers, single-shot and continuous conversion, MUX, PGA, conversion time 1/DR with optional rate deviation | comparator/ALERT, wake-up time |
| Microchip MCP4728 DAC | MSOP-10 | all write commands except address bits, register read, LDAC/UDAC/general call, output voltage | address bit commands, EEPROM write time |
| 24C32 EEPROM | 8-pin SOIC/TSSOP/MSOP/DIP | addressing, page writes, sequential reads, write cycle (NACK until t_WR ends) | WP pin |

Each model is tested with an independent open-source Rust driver for the chip (`ads1x1x`,
`mcp4728`, `eeprom24x`) and with the byte sequences the Linux driver sends.

## Chip file

```toml
name = "TI ADS1115"
datasheet = "https://www.ti.com/lit/ds/symlink/ads1115.pdf"
bus = "i2c"

[address]                        # fixed | strap | bits
kind = "strap"
pin = "addr"
levels = { gnd = 0x48, vdd = 0x49, sda = 0x4a, scl = 0x4b }

[devicetree]                     # points at the Linux binding, adds pin knowledge
compatible = ["ti,ads1115"]
binding = "Documentation/devicetree/bindings/iio/adc/ti,ads1015.yaml"
driver = "drivers/iio/adc/ti-ads1015.c"
gpios = { interrupts = "alert" }

[[package]]                      # one per package: pinouts differ
name = "VSSOP-10"
mpn = ["ADS1115IDGS"]            # prefixes, specific enough to select the package
pins = { addr = "1", alert = "2", gnd = "3", vdd = "8", sda = "9", scl = "10" }

[sim]
model = "ads1115"                # a model in src/sim
covers = ["..."]
missing = ["..."]
```

Loading validates the file: bus pins exist in every package, address and GPIO pin roles exist,
strap levels are `gnd`, `vdd` or a pin role, compatibles look like `vendor,device`, and the model
exists. Part number prefixes must not overlap between chips.

## Using the models

```rust
use weft_parts::sim::{Ads1115, I2cBus, Message};

let mut bus = I2cBus::new();
let mut adc = Ads1115::new();
adc.set_input(0, 1.0);
bus.attach(0x48, Box::new(adc))?;

// Linux i2c_msg semantics:
let mut value = [0u8; 2];
bus.transfer(0x48, &mut [Message::Write(&[0x01]), Message::Read(&mut value)])?;

// Or hand `&mut bus` to any embedded-hal 1.0 I2C driver.
```

## Time

Models are time-aware but never read a clock. Before every bus event the bus tells each model
the time (`I2cTarget::advance`), and the model finishes whatever is due: a conversion, an EEPROM
write cycle. Where the time comes from is the caller's choice:

| Clock | Time moves | Used by |
|---|---|---|
| `VirtualClock` | only with bus traffic and driver delays (it implements embedded-hal `DelayNs`); deterministic | host tests, later co-simulation |
| `HostClock` | with the monotonic clock of the machine | weft-sim, inside the guest |

A transfer lasts as long as its bits at the bus speed (100 kHz by default, `I2cBus::set_speed`):
start and address 10 SCL periods, each byte 9, stop 1. A driver that polls a busy bit therefore
sees the chip finish after the right amount of time.

```rust
use std::time::Duration;
use weft_parts::sim::{At24, I2cBus, Message, VirtualClock};

let clock = VirtualClock::new();
let mut bus = I2cBus::with_clock(clock.clone());
bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))?;
bus.transfer(0x50, &mut [Message::Write(&[0x00, 0x00, 0xab])])?;
assert!(bus.transfer(0x50, &mut [Message::Write(&[])]).is_err()); // busy for t_WR
clock.advance(Duration::from_millis(5));
assert!(bus.transfer(0x50, &mut [Message::Write(&[])]).is_ok());
```

Each model keeps its own timing in a `ClockDomain` (its oscillator, conversion rate or SCL), with
an optional deviation in ppm so datasheet tolerances can be tested. Edges are computed from a
fixed origin, so long runs do not drift. `next_event` tells a scheduler when a model changes state
on its own.

`sim::build` creates the model a chip file names, so a backend can attach chips by part number.

## Adding a chip

1. Add `chips/<vendor>/<chip>.toml` from the datasheet and list it in `src/library.rs`.
2. For a model, add `src/sim/<model>.rs`, register it in `sim::build`, and test it against an
   independent driver where one exists, plus the Linux driver's transfers.
3. Model the datasheet timing that drivers can observe (busy bits, NACK while busy, conversion
   times) through `advance` and `next_event`, with the maximum datasheet value unless a test
   sets a deviation. List what is not modelled under `missing`.

## Development

`nix develop` (or direnv) provides the pinned toolchain and CI tools; `just ci` runs everything CI
runs.

## Licence

MIT. See [LICENSE](LICENSE).
