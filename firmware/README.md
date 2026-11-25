# Firmware

C++ (Arduino, PlatformIO) for the **Waveshare Universal e-Paper Driver Board** (ESP8266) with the **4.2" raw panel** (400×300, black/white). The firmware stays small on purpose: the server does all rendering, the board only shows the frame.

Every 30 minutes the board:
1. wakes from deep sleep and connects to WiFi (15 s limit);
2. fetches `/display.bin` over HTTPS with the last `ETag` (`If-None-Match`);
3. on `304` does nothing; on `200` streams the 15 000 bytes straight into the panel's RAM and refreshes **only if the frame arrived completely**, then remembers the new ETag (RTC memory);
4. puts the panel into hibernation and goes back to deep sleep for 30 minutes, also after any error. The download (TLS, request, headers and body) has 15 s in total, and every received byte is checked against that deadline. A timer ends the whole wake after 45 s, whatever is blocking. A refresh that times out is not remembered, so the next wake tries again.

An interrupted download never changes the picture: the first bytes don't clear the panel, and the refresh only starts after all 15 000 bytes.

TLS runs without certificate check: the image is public and a man-in-the-middle could only show a wrong picture (see `docs/PLAN.md`).

## Setup

```sh
cd firmware
cp include/secrets.example.h include/secrets.h   # WiFi + URL, gitignored
pio run -t upload                                # board on USB
pio device monitor                               # logs (115200 baud)
```

## Hardware notes (to verify on the board)

- **Board revision:** there are two revisions of the Waveshare e-Paper ESP8266 Driver Board.
  - **New** (default): CS 15, DC 4, **RST 2, BUSY 5**, SPI CLK 14, DIN 13. GPIO16 is free.
  - **Old**: **BUSY 16, RST 5**. Build with `-D BOARD_REV_OLD`. GPIO16 is taken by BUSY, so the board can't wake from deep sleep: the firmware then waits 30 minutes with WiFi off and restarts. That needs USB power. If a wake hits the 45 s limit, the board restarts and first waits out the 30 minutes (flag in RTC memory).
  - Check which pin BUSY is wired to before adding any wire.
- **Panel version:** V1 panels use `GxEPD2_420`. If the panel says **V2** (SSD1683), enable `-D PANEL_420_V2` in `platformio.ini`.
- **Deep sleep (new revision only):** waking up needs **GPIO16 connected to RST**. **Never** add this wire on the old revision (it would connect panel BUSY to reset).
- **Colours:** if the image appears inverted, the panel expects the opposite bit polarity; tell me and I'll flip it in the firmware.
