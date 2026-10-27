# musb 0.3.1 with WB32FQ95 support

Vendored from https://github.com/decaday/musb at v0.3.1
(`49503a0d4c51723a66158ec1db8b2d476f6fc592`), without `musb-readconf/`.
Plan: upstream these changes as a PR, then switch back to the crates.io/git
dependency.

## Added: `builtin-wb32fq95` profile

- `registers/profiles/wb32fq95.yaml`, `registers/blocks/wb32.yaml`: the
  layout of `USB_TypeDef` in the vendor header `wb32fq95xx.h` (an 8-bit
  "lite" layout: INTRIN 0x02, INTROUT 0x04, INTRUSB 0x06, enables at
  0x07/0x09/0x0B, indexed CSRs at 0x10-0x17, FIFOs at 0x20 with a 4-byte
  stride). 4 endpoints of 64 bytes, IN and OUT FIFOs separate.
- `src/prebuilds/wb32fq95/`: written by hand from the py32f07x prebuild
  (`yaml2pac` isn't installed here), changing only the offsets, the endpoint
  count, and FRAME/TXMAXP/RXMAXP/RXCOUNT, which are single bytes on WB32: u8
  storage behind the same u16 accessors, so the driver code is unchanged.
  Regenerating with `update_prebuild.py` should give the same offsets.

## Fixes (apply to every profile)

- `alloc_endpoint.rs`: without `_ep-shared-fifo`, IN and OUT of one endpoint
  number may have different types (only isochronous still needs a number to
  itself). Before, a bulk endpoint couldn't share its number at all, so CDC-ACM
  plus one HID interface didn't fit in EP1-3.
- `bus.rs`: a bus reset clears the enabled state of every endpoint but EP0,
  and disabling an endpoint wakes its waiters.
- `endpoint.rs`: `read`/`write` on a disabled endpoint return
  `EndpointError::Disabled` (as other embassy-usb drivers do) instead of
  touching its FIFO. Before, a write pending across a bus reset loaded the
  FIFO of an endpoint whose MAXP the reset had cleared.
- `bus.rs`: when suspend and resume are latched by one interrupt, report
  Suspend first; the reverse order left embassy-usb believing it was suspended.

All four were found by the USB model in `sim/` (`sim/tests/m3_usb.robot`).

Later (found by review against ChibiOS's `hal_usb_lld.c`, reproduced in the
sim by `m4_rmk.robot` "Odd Host Requests Don't Crash USB"):

- `bus.rs`: `endpoint_set_stalled`/`endpoint_is_stalled` ignore endpoint
  numbers the chip doesn't have. The number comes straight from the host's
  `SET/CLEAR_FEATURE(ENDPOINT_HALT)` wIndex, and indexed the waker arrays
  (a panic). Stalling an OUT endpoint wakes its RX waiter only. The reset
  flush uses plain `txcsrl` writes.
- `control_pipe.rs`, EP0 as ChibiOS's `usb_serve_ep0_handler`: `setup()`
  clears SentStall and a stale SetupEnd; `data_in`/`data_out` return
  `Disabled` when the host abandoned the transfer (SetupEnd, a new SETUP
  waiting, or a bus reset) instead of loading the FIFO or reading the new
  SETUP as data; `accept_set_address` gives up on a bus reset. The
  interrupt counts bus resets (`RESET_GEN`) and wakes EP0, because
  embassy-usb doesn't poll the bus during a control transfer. Before, an
  abandoned control read left EP0 wedged.

## Not here (in `wb32-hal/src/usb.rs`)

Clocks, PHY, the D+ attach and the `POWER.SUSEN` / `INTRUSBE.SUSIE`
re-arm after each bus reset are chip glue, ported from ChibiOS-contrib.
