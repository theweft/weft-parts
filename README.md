# weft-parts

> **Status: experimental (0.0.x).** APIs and formats change without notice. Not for production use yet.

Machine-readable chip data and behavioural chip models, part of [weft](https://theweft.org).
One TOML file per chip under [chips/](chips/) serves three jobs:

| Job | Uses | Consumer |
|---|---|---|
| Check PCB wiring | packages, part numbers, pinouts, address straps | [weft-boardchecker](https://github.com/theweft/weft-boardchecker) against IPC-2581 netlists |
| Check device trees | compatible strings, Linux binding and driver, GPIO property to pin | weft-boardchecker; [dt-schema](https://github.com/devicetree-org/dt-schema) validates against the binding itself |
| Simulate the chip | behavioural model behind I2C transfers | Linux drivers (QEMU virtio-i2c backend, planned), Rust MCU drivers via embedded-hal, unit tests |

"Digital twin" is close; "behavioural model" or "bus-level model" is the usual term in
simulation. The models answer the bus traffic the way the chip does and expose analog inputs and
outputs to the test, but do not model timing or electrical behaviour.

## Chips

| Chip | Packages | Model covers | Model does not cover |
|---|---|---|---|
| TI ADS1115 ADC | VSSOP-10 | registers, single-shot and continuous conversion, MUX, PGA | comparator/ALERT, conversion time |
| Microchip MCP4728 DAC | MSOP-10 | all write commands except address bits, register read, LDAC/UDAC/general call, output voltage | address bit commands, EEPROM write time |
| 24C32 EEPROM | 8-pin SOIC/TSSOP/MSOP/DIP | addressing, page writes, sequential reads | write cycle time, WP pin |

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

`sim::build` creates the model a chip file names, so a backend can attach chips by part number.

## Adding a chip

1. Add `chips/<vendor>/<chip>.toml` from the datasheet and list it in `src/library.rs`.
2. For a model, add `src/sim/<model>.rs`, register it in `sim::build`, and test it against an
   independent driver where one exists, plus the Linux driver's transfers.

## Development

`nix develop` (or direnv) provides the pinned toolchain and CI tools; `just ci` runs everything CI
runs.

## Licence

MIT. See [LICENSE](LICENSE).
