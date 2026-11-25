// running-viz e-paper client.
//
// Every wake-up: connect to WiFi, fetch the server-rendered frame (400x300, 1 bit,
// 15000 bytes, MSB first, 1 = white) with the last ETag, draw it only if it changed and
// arrived completely in time, then sleep for 30 minutes - also after any failure.
//
// Deep sleep wake-up needs GPIO16 wired to RST (new board revision, see README).

#include <Arduino.h>
#include <ESP8266HTTPClient.h>
#include <ESP8266WiFi.h>
#include <GxEPD2_BW.h>
#include <Ticker.h>
#include <WiFiClientSecureBearSSL.h>

#include "secrets.h"

// Waveshare e-Paper ESP8266 Driver Board pins (CLK 14, DIN 13 are the hardware SPI pins).
#ifdef BOARD_REV_OLD
// old revision: BUSY sits on GPIO16, so deep sleep cannot be woken up (see README)
static const int PIN_CS = 15, PIN_DC = 4, PIN_RST = 5, PIN_BUSY = 16;
#define NO_DEEP_SLEEP
#else
static const int PIN_CS = 15, PIN_DC = 4, PIN_RST = 2, PIN_BUSY = 5;
#endif

#ifdef PANEL_420_V2
using Panel = GxEPD2_420_GDEY042T81;  // SSD1683
static const int BUSY_ACTIVE = HIGH;
#else
using Panel = GxEPD2_420;  // UC8176
static const int BUSY_ACTIVE = LOW;
#endif
// The frame is streamed straight into the panel's RAM, so a tiny page buffer is enough.
GxEPD2_BW<Panel, 8> display(Panel(PIN_CS, PIN_DC, PIN_RST, PIN_BUSY));

static const uint16_t WIDTH = 400, HEIGHT = 300;
static const size_t FRAME_BYTES = WIDTH * HEIGHT / 8;  // 15000
static const uint16_t ROWS_PER_CHUNK = 8;              // 400 bytes per chunk
static const uint64_t SLEEP_US = 30ULL * 60 * 1000 * 1000;
static const uint32_t WIFI_TIMEOUT_MS = 15000;
static const uint32_t HTTP_TIMEOUT_MS = 15000;     // TLS + headers + body, in total
static const uint32_t WAKE_BUDGET_MS = 45000;      // whole wake, enforced in code
static const uint32_t REFRESH_RESERVE_MS = 10000;  // time a full refresh may need
static const uint32_t HARD_LIMIT_MS = WAKE_BUDGET_MS + 15000;  // last-resort cut-off

// The last shown ETag survives deep sleep in RTC memory.
struct RtcState {
  uint32_t magic;
  char etag[40];
};
static const uint32_t RTC_MAGIC = 0x52564931;  // "RVI1"
static RtcState rtc;
static Ticker hardLimit;

static void loadState() {
  if (!ESP.rtcUserMemoryRead(0, reinterpret_cast<uint32_t*>(&rtc), sizeof(rtc)) || rtc.magic != RTC_MAGIC) {
    memset(&rtc, 0, sizeof(rtc));
    rtc.magic = RTC_MAGIC;
  }
}

static void saveState() { ESP.rtcUserMemoryWrite(0, reinterpret_cast<uint32_t*>(&rtc), sizeof(rtc)); }

static void sleepCycle() {
#ifdef NO_DEEP_SLEEP
  // Without the GPIO16 -> RST wake-up wire: radio off, wait, restart (needs USB power).
  WiFi.mode(WIFI_OFF);
  delay(SLEEP_US / 1000);
  ESP.restart();
#else
  ESP.deepSleep(SLEEP_US);
#endif
}

[[noreturn]] static void sleepNow(const char* why) {
  hardLimit.detach();
  Serial.printf("sleeping 30 min (%s)\n", why);
  WiFi.disconnect(true);
  sleepCycle();
  for (;;) delay(1000);
}

static uint32_t remaining(uint32_t deadline) {
  const uint32_t now = millis();
  return now >= deadline ? 0 : deadline - now;
}

static bool connectWifi() {
  WiFi.persistent(false);
  WiFi.mode(WIFI_STA);
  WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
  const uint32_t start = millis();
  while (WiFi.status() != WL_CONNECTED) {
    if (millis() - start > WIFI_TIMEOUT_MS) return false;
    delay(100);
  }
  return true;
}

// Reads exactly `len` bytes before `deadline`, or fails (stream ended or too late).
static bool readExactly(WiFiClient* stream, uint8_t* buf, size_t len, uint32_t deadline) {
  size_t got = 0;
  while (got < len) {
    if (remaining(deadline) == 0) return false;
    if (!stream->connected() && stream->available() == 0) return false;
    const int n = stream->read(buf + got, len - got);
    if (n > 0) {
      got += n;
    } else {
      delay(5);
    }
  }
  return true;
}

void setup() {
  // Last resort if a blocking call (TLS, panel BUSY wait) overruns every soft deadline.
  hardLimit.once_ms(HARD_LIMIT_MS, [] {
#ifdef NO_DEEP_SLEEP
    ESP.restart();  // a timer callback must not block for 30 minutes
#else
    ESP.deepSleep(SLEEP_US);
#endif
  });
  Serial.begin(115200);
  Serial.println("\nrunning-viz");
  loadState();

  if (!connectWifi()) sleepNow("no wifi");

  const uint32_t wakeDeadline = WAKE_BUDGET_MS;  // millis() counts from boot
  const uint32_t httpDeadline = min<uint32_t>(millis() + HTTP_TIMEOUT_MS, wakeDeadline - REFRESH_RESERVE_MS);
  if (remaining(httpDeadline) == 0) sleepNow("no time left");

  // TLS without certificate check: the image is public and the worst case of a
  // man-in-the-middle is a wrong picture (decision in docs/PLAN.md).
  BearSSL::WiFiClientSecure client;
  client.setInsecure();
  client.setTimeout(remaining(httpDeadline));
  HTTPClient http;
  http.setTimeout(remaining(httpDeadline));
  if (!http.begin(client, DISPLAY_URL)) sleepNow("bad url");
  const char* keep[] = {"ETag"};
  http.collectHeaders(keep, 1);
  if (rtc.etag[0]) http.addHeader("If-None-Match", rtc.etag);

  const int code = http.GET();
  if (remaining(httpDeadline) == 0) sleepNow("http too slow");
  if (code == HTTP_CODE_NOT_MODIFIED) sleepNow("unchanged");
  if (code != HTTP_CODE_OK) {
    Serial.printf("http %d\n", code);
    sleepNow("http error");
  }
  if (http.getSize() != (int)FRAME_BYTES) sleepNow("unexpected size");
  const String etag = http.header("ETag");
  WiFiClient* stream = http.getStreamPtr();
  if (stream == nullptr) sleepNow("connection closed");

  // Stream the frame into the panel RAM chunk by chunk; nothing is shown until refresh().
  // initial = false: no clearing refresh on the first write, so an incomplete download
  // leaves the previous image on the panel.
  display.init(115200, false, 2, false);
  static uint8_t chunk[WIDTH / 8 * ROWS_PER_CHUNK];
  for (uint16_t y = 0; y < HEIGHT; y += ROWS_PER_CHUNK) {
    const uint16_t rows = min<uint16_t>(ROWS_PER_CHUNK, HEIGHT - y);
    if (!readExactly(stream, chunk, WIDTH / 8 * rows, httpDeadline)) {
      display.hibernate();
      sleepNow("incomplete frame");
    }
    display.epd2.writeImage(chunk, 0, y, WIDTH, rows);
  }
  http.end();

  if (remaining(wakeDeadline) < REFRESH_RESERVE_MS) {
    display.hibernate();
    sleepNow("no time left for refresh");
  }
  display.epd2.refresh(false);  // full refresh: crisp, no ghosting
  // GxEPD2 returns normally after a BUSY timeout; a panel that is still busy did not finish.
  const bool shown = digitalRead(PIN_BUSY) != BUSY_ACTIVE;
  display.hibernate();
  if (!shown) sleepNow("panel refresh timed out");

  // Remember the frame only after it is really on the panel.
  strncpy(rtc.etag, etag.c_str(), sizeof(rtc.etag) - 1);
  rtc.etag[sizeof(rtc.etag) - 1] = '\0';
  saveState();
  sleepNow("updated");
}

void loop() {}
