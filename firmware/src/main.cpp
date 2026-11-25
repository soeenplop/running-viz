// running-viz e-paper client.
//
// Every wake-up: connect to WiFi, fetch the server-rendered frame (400x300, 1 bit,
// 15000 bytes, MSB first, 1 = white) with the last ETag, draw it only if it changed and
// arrived completely in time, then sleep for 30 minutes - also after any failure.
//
// Deep sleep wake-up needs GPIO16 wired to RST (new board revision, see README).

#include <Arduino.h>
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
static const uint32_t HTTP_TIMEOUT_MS = 15000;     // TLS + request + headers + body, in total
static const uint32_t WAKE_BUDGET_MS = 45000;      // whole wake; also the hard cut-off
static const uint32_t REFRESH_RESERVE_MS = 10000;  // time a full refresh may need

// Survives deep sleep and restarts: the last shown ETag and a pending cooldown.
struct RtcState {
  uint32_t magic;
  uint32_t cooldown;  // old board: set by the hard cut-off, consumed on the next boot
  char etag[40];
};
static const uint32_t RTC_MAGIC = 0x52564932;  // "RVI2"
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

// Hard cut-off: whatever is blocking (DNS, TLS, panel BUSY wait), the wake ends when it fires.
static void cutoff() {
#ifdef NO_DEEP_SLEEP
  rtc.cooldown = 1;  // a timer callback must not block: restart and wait on next boot
  saveState();
  ESP.restart();
#else
  ESP.deepSleep(SLEEP_US);
#endif
}

static void armCutoff(uint32_t inMs) {
  hardLimit.detach();
  hardLimit.once_ms(inMs > 0 ? inMs : 1, cutoff);
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

// Reads exactly `len` bytes before `deadline`, or fails (connection closed or too late).
static bool readExactly(WiFiClient& c, uint8_t* buf, size_t len, uint32_t deadline) {
  size_t got = 0;
  while (got < len) {
    if (remaining(deadline) == 0) return false;
    if (c.available() == 0) {
      if (!c.connected()) return false;
      delay(2);
      continue;
    }
    const int n = c.read(buf + got, len - got);
    if (n > 0) got += n;
  }
  return true;
}

// Reads one header line (without CRLF) before `deadline`. Fails on timeout, close, a
// line longer than 256 bytes or a CR that is not followed by LF (malformed response).
static bool readLine(WiFiClient& c, String& line, uint32_t deadline) {
  line = "";
  bool cr = false;
  while (true) {
    uint8_t b;
    if (!readExactly(c, &b, 1, deadline)) return false;
    if (cr) return b == '\n';
    if (b == '\r') {
      cr = true;
      continue;
    }
    if (b == '\n') return true;
    if (line.length() >= 256) return false;
    line += static_cast<char>(b);
  }
}

struct Response {
  int status = 0;
  long length = -1;     // -1: missing, invalid or contradictory
  bool lengthSeen = false;
  bool encoded = false;  // any Transfer-Encoding (e.g. chunked): not supported
  String etag;
};

// A Content-Length value: digits only, no sign, no trailing garbage.
static long parseLength(const String& v) {
  if (v.isEmpty() || v.length() > 9) return -1;
  for (size_t i = 0; i < v.length(); ++i) {
    if (!isDigit(v[i])) return -1;
  }
  return v.toInt();
}

// Minimal HTTP/1.1 GET: every byte of the status line and headers counts against the
// same absolute deadline (unlike HTTPClient, whose per-line timeouts restart).
static bool get(WiFiClient& c, uint32_t deadline, Response& r) {
  c.setTimeout(remaining(deadline));
  if (!c.connect(DISPLAY_HOST, 443) || remaining(deadline) == 0) return false;
  String req = String("GET ") + DISPLAY_PATH + " HTTP/1.1\r\nHost: " + DISPLAY_HOST +
               "\r\nUser-Agent: running-viz\r\nConnection: close\r\n";
  if (rtc.etag[0]) req += String("If-None-Match: ") + rtc.etag + "\r\n";
  req += "\r\n";
  c.print(req);

  String line;
  if (!readLine(c, line, deadline) || !line.startsWith("HTTP/1.")) return false;
  r.status = line.substring(9, 12).toInt();
  while (readLine(c, line, deadline)) {
    if (line.isEmpty()) return true;  // end of headers
    const int colon = line.indexOf(':');
    if (colon < 0) continue;
    String name = line.substring(0, colon);
    name.toLowerCase();
    String value = line.substring(colon + 1);
    value.trim();
    if (name == "content-length") {
      const long len = parseLength(value);
      // a repeated header must agree with the first one
      r.length = (r.lengthSeen && len != r.length) ? -1 : len;
      r.lengthSeen = true;
    }
    if (name == "etag") r.etag = value;
    if (name == "transfer-encoding") r.encoded = true;
  }
  return false;
}

void setup() {
  loadState();
#ifdef NO_DEEP_SLEEP
  if (rtc.cooldown) {
    // the previous wake hit the hard cut-off: keep the 30-minute interval
    rtc.cooldown = 0;
    saveState();
    sleepCycle();
  }
#endif
  armCutoff(WAKE_BUDGET_MS);
  Serial.begin(115200);
  Serial.println("\nrunning-viz");

  if (!connectWifi()) sleepNow("no wifi");

  const uint32_t httpDeadline = min<uint32_t>(millis() + HTTP_TIMEOUT_MS, WAKE_BUDGET_MS - REFRESH_RESERVE_MS);
  if (remaining(httpDeadline) == 0) sleepNow("no time left");

  // TLS without certificate check: the image is public and the worst case of a
  // man-in-the-middle is a wrong picture (decision in docs/PLAN.md).
  BearSSL::WiFiClientSecure client;
  client.setInsecure();
  // DNS, TCP and the TLS handshake have their own internal timeouts: the cut-off moves
  // to the download deadline while the network is used, then back to the wake budget.
  armCutoff(remaining(httpDeadline));
  Response res;
  if (!get(client, httpDeadline, res)) sleepNow("http failed or too slow");
  if (res.status == 304) sleepNow("unchanged");
  if (res.status != 200) {
    Serial.printf("http %d\n", res.status);
    sleepNow("http error");
  }
  if (res.encoded || res.length != (long)FRAME_BYTES) sleepNow("unexpected framing or size");

  // Stream the frame into the panel RAM chunk by chunk; nothing is shown until refresh().
  // initial = false: no clearing refresh on the first write, so an incomplete download
  // leaves the previous image on the panel.
  display.init(115200, false, 2, false);
  static uint8_t chunk[WIDTH / 8 * ROWS_PER_CHUNK];
  for (uint16_t y = 0; y < HEIGHT; y += ROWS_PER_CHUNK) {
    const uint16_t rows = min<uint16_t>(ROWS_PER_CHUNK, HEIGHT - y);
    if (!readExactly(client, chunk, WIDTH / 8 * rows, httpDeadline)) {
      display.hibernate();
      sleepNow("incomplete frame");
    }
    display.epd2.writeImage(chunk, 0, y, WIDTH, rows);
  }
  client.stop();
  armCutoff(remaining(WAKE_BUDGET_MS));

  if (remaining(WAKE_BUDGET_MS) < REFRESH_RESERVE_MS) {
    display.hibernate();
    sleepNow("no time left for refresh");
  }
  display.epd2.refresh(false);  // full refresh: crisp, no ghosting
  // GxEPD2 returns normally after a BUSY timeout; a panel that is still busy did not finish.
  const bool shown = digitalRead(PIN_BUSY) != BUSY_ACTIVE;
  display.hibernate();
  if (!shown) sleepNow("panel refresh timed out");

  // Remember the frame only after it is really on the panel.
  strncpy(rtc.etag, res.etag.c_str(), sizeof(rtc.etag) - 1);
  rtc.etag[sizeof(rtc.etag) - 1] = '\0';
  saveState();
  sleepNow("updated");
}

void loop() {}
