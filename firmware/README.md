# Firmware

C++ (Arduino, PlatformIO) for the **Waveshare Universal e-Paper Driver Board** (ESP8266) with the **4.2" raw panel** (400×300, black/white). The firmware stays small on purpose: the server does all rendering, the board only shows the frame.

Every 30 minutes the board:
1. wakes from deep sleep and connects to WiFi (15 s limit);
2. fetches `/display.bin` over HTTPS with the last `ETag` (`If-None-Match`);
3. on `304` does nothing; on `200` streams the 15 000 bytes straight into the panel's RAM and refreshes **only if the frame arrived completely**, then remembers the new ETag (RTC memory);
4. puts the panel into hibernation and goes back to deep sleep for 30 minutes, also after any error (whole wake ≤ 45 s).

TLS runs without certificate check: the image is public and a man-in-the-middle could only show a wrong picture (see `docs/PLAN.md`).

## Setup

```sh
cd firmware
cp include/secrets.example.h include/secrets.h   # WiFi + URL, gitignored
pio run -t upload                                # board on USB
pio device monitor                               # logs (115200 baud)
```

## Hardware notes (to verify on the board)

- **Pins** (Waveshare driver board): CS 15, DC 4, RST 2, BUSY 5, SPI CLK 14, DIN 13.
- **Panel version:** V1 panels use `GxEPD2_420`. If the panel says **V2** (SSD1683), enable `-D PANEL_420_V2` in `platformio.ini`.
- **Deep sleep:** waking up needs **GPIO16 connected to RST**. If the board doesn't wake up after 30 minutes, add that wire (or run on USB power).
- **Colours:** if the image appears inverted, the panel expects the opposite bit polarity; tell me and I'll flip it in the firmware.
