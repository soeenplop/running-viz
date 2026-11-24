// running-viz e-paper client.
//
// Every wake-up: connect to WiFi, fetch the server-rendered frame (400x300, 1 bit,
// 15000 bytes, MSB first, 1 = white) with the last ETag, draw it only if it changed,
// then deep sleep for 30 minutes - also after any failure.
//
// Deep sleep wake-up needs GPIO16 wired to RST.

#include <Arduino.h>
#include <ESP8266HTTPClient.h>
#include <ESP8266WiFi.h>
#include <GxEPD2_BW.h>
#include <WiFiClientSecureBearSSL.h>

#include "secrets.h"

// Waveshare e-Paper ESP8266 Driver Board pins
static const int PIN_CS = 15, PIN_DC = 4, PIN_RST = 2, PIN_BUSY = 5;

#ifdef PANEL_420_V2
using Panel = GxEPD2_420_GDEY042T81;
#else
using Panel = GxEPD2_420;
#endif
// The frame is streamed straight into the panel's RAM, so a tiny page buffer is enough.
GxEPD2_BW<Panel, 8> display(Panel(PIN_CS, PIN_DC, PIN_RST, PIN_BUSY));

static const uint16_t WIDTH = 400, HEIGHT = 300;
static const size_t FRAME_BYTES = WIDTH * HEIGHT / 8;  // 15000
static const uint16_t ROWS_PER_CHUNK = 8;              // 400 bytes per chunk
static const uint64_t SLEEP_US = 30ULL * 60 * 1000 * 1000;
static const uint32_t WIFI_TIMEOUT_MS = 15000, HTTP_TIMEOUT_MS = 15000, WAKE_BUDGET_MS = 45000;

// The last shown ETag survives deep sleep in RTC memory.
struct RtcState {
  uint32_t magic;
  char etag[40];
};
static const uint32_t RTC_MAGIC = 0x52564931;  // "RVI1"
static RtcState rtc;

static void loadState() {
  if (!ESP.rtcUserMemoryRead(0, reinterpret_cast<uint32_t*>(&rtc), sizeof(rtc)) || rtc.magic != RTC_MAGIC) {
    memset(&rtc, 0, sizeof(rtc));
    rtc.magic = RTC_MAGIC;
  }
}

static void saveState() { ESP.rtcUserMemoryWrite(0, reinterpret_cast<uint32_t*>(&rtc), sizeof(rtc)); }

[[noreturn]] static void sleepNow(const char* why) {
  Serial.printf("sleeping 30 min (%s)\n", why);
  WiFi.disconnect(true);
  ESP.deepSleep(SLEEP_US);
  for (;;) delay(1000);
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

// Reads exactly `len` bytes or fails (stream ended, timeout or wake budget exceeded).
static bool readExactly(WiFiClient* stream, uint8_t* buf, size_t len) {
  size_t got = 0;
  uint32_t last = millis();
  while (got < len) {
    if (millis() > WAKE_BUDGET_MS || millis() - last > HTTP_TIMEOUT_MS) return false;
    const int n = stream->read(buf + got, len - got);
    if (n > 0) {
      got += n;
      last = millis();
    } else {
      delay(5);
    }
  }
  return true;
}

void setup() {
  Serial.begin(115200);
  Serial.println("\nrunning-viz");
  loadState();

  if (!connectWifi()) sleepNow("no wifi");

  // TLS without certificate check: the image is public and the worst case of a
  // man-in-the-middle is a wrong picture (decision in docs/PLAN.md).
  BearSSL::WiFiClientSecure client;
  client.setInsecure();
  HTTPClient http;
  http.setTimeout(HTTP_TIMEOUT_MS);
  if (!http.begin(client, DISPLAY_URL)) sleepNow("bad url");
  const char* keep[] = {"ETag"};
  http.collectHeaders(keep, 1);
  if (rtc.etag[0]) http.addHeader("If-None-Match", rtc.etag);

  const int code = http.GET();
  if (code == HTTP_CODE_NOT_MODIFIED) sleepNow("unchanged");
  if (code != HTTP_CODE_OK) {
    Serial.printf("http %d\n", code);
    sleepNow("http error");
  }
  if (http.getSize() != (int)FRAME_BYTES) sleepNow("unexpected size");
  const String etag = http.header("ETag");

  // Stream the frame into the panel RAM chunk by chunk; nothing is shown until refresh().
  display.init(115200, true, 2, false);
  WiFiClient* stream = http.getStreamPtr();
  static uint8_t chunk[WIDTH / 8 * ROWS_PER_CHUNK];
  for (uint16_t y = 0; y < HEIGHT; y += ROWS_PER_CHUNK) {
    const uint16_t rows = min<uint16_t>(ROWS_PER_CHUNK, HEIGHT - y);
    if (!readExactly(stream, chunk, WIDTH / 8 * rows)) {
      display.hibernate();
      sleepNow("incomplete frame");
    }
    display.epd2.writeImage(chunk, 0, y, WIDTH, rows);
  }
  http.end();

  display.epd2.refresh(false);  // full refresh: crisp, no ghosting
  display.hibernate();

  // Remember the frame only after it is really on the panel.
  strncpy(rtc.etag, etag.c_str(), sizeof(rtc.etag) - 1);
  rtc.etag[sizeof(rtc.etag) - 1] = '\0';
  saveState();
  sleepNow("updated");
}

void loop() {}
