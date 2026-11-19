#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XShm.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <sys/shm.h>
#include <sys/resource.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1000.0 + t.tv_nsec / 1e6;
}
static void until(double ms) {
  struct timespec t = {(time_t)(ms / 1000), (long)((ms - (long)(ms / 1000) * 1000) * 1e6)};
  clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &t, NULL);
}
static int component(unsigned long p, unsigned long mask) {
  if (!mask) return 0;
  while (!(mask & 1)) { mask >>= 1; p >>= 1; }
  return (int)((p & mask) * 255 / mask);
}
static int focus(Display *d) {
  const char *requested = getenv("BENCH_WINDOW");
  if (requested && *requested) {
    XSetInputFocus(d, strtoul(requested, NULL, 0), RevertToParent, CurrentTime);
    XSync(d, False); return 1;
  }
  Window root = DefaultRootWindow(d), parent, returned, *children = NULL, chosen = None;
  unsigned count = 0; long area = 0;
  if (!XQueryTree(d, root, &returned, &parent, &children, &count)) return 0;
  for (unsigned i = 0; i < count; i++) {
    XWindowAttributes a;
    if (XGetWindowAttributes(d, children[i], &a) && a.map_state == IsViewable && a.class == InputOutput && (long)a.width * a.height >= area) {
      area = (long)a.width * a.height; chosen = children[i];
    }
  }
  if (children) XFree(children);
  if (chosen == None) return 0;
  XSetInputFocus(d, chosen, RevertToParent, CurrentTime);
  XSync(d, False);
  return 1;
}
int main(int argc, char **argv) {
  setvbuf(stdout, NULL, _IOFBF, 65536);
  if (argc == 3 && !strcmp(argv[1], "--focus")) {
    Display *d = XOpenDisplay(argv[2]);
    if (!d) return 2;
    int ok = focus(d); XCloseDisplay(d); return ok ? 0 : 2;
  }
  if (argc != 5) { fprintf(stderr, "usage: pixels DISPLAY seconds trials seed\n"); return 2; }
  Display *d = XOpenDisplay(argv[1]);
  if (!d || !XShmQueryExtension(d)) { fprintf(stderr, "XShm display unavailable\n"); return 2; }
  if (!focus(d)) return 2;
  int screen = DefaultScreen(d), width = DisplayWidth(d, screen);
  XShmSegmentInfo shm = {0};
  int height = DisplayHeight(d, screen);
  if (width != 1280 || height != 720) { fprintf(stderr, "Client display must be 1280x720\n"); XCloseDisplay(d); return 2; }
  XImage *im = XShmCreateImage(d, DefaultVisual(d, screen), DefaultDepth(d, screen), ZPixmap, NULL, &shm, width, height);
  if (!im) return 2;
  shm.shmid = shmget(IPC_PRIVATE, im->bytes_per_line * height, IPC_CREAT | 0600);
  if (shm.shmid < 0) return 2;
  shm.shmaddr = shmat(shm.shmid, NULL, 0);
  if (shm.shmaddr == (void *)-1) { shmctl(shm.shmid, IPC_RMID, NULL); return 2; }
  im->data = shm.shmaddr;
  if (!XShmAttach(d, &shm)) return 2;
  XSync(d, False);
  shmctl(shm.shmid, IPC_RMID, NULL);
  if (!XShmGetImage(d, RootWindow(d, screen), im, 0, 0, AllPlanes)) return 3;
  int row = -1;
  for (int y = 0; y < height && row < 0; y += 4) {
    int cyan = 0, magenta = 0;
    for (int x = 0; x < width; x++) {
      unsigned long p = XGetPixel(im, x, y);
      int r = component(p, im->red_mask), g = component(p, im->green_mask), b = component(p, im->blue_mask);
      if (r < 80 && g > 150 && b > 150) cyan++;
      if (cyan >= 20 && r > 150 && g < 80 && b > 150) magenta++;
    }
    if (cyan >= 20 && magenta >= 20) row = y + 8 < height ? y + 8 : y;
  }
  if (row < 0) { fprintf(stderr, "No marker color guards on client display\n"); return 3; }
  im->height = 1;
  printf("M %d\n", row);
  double start = now(), deadline = start, next_input = start + 2000;
  int trials = atoi(argv[3]), sent = 0;
  srand((unsigned)strtoul(argv[4], NULL, 10));
  KeyCode space = XKeysymToKeycode(d, XK_space);
  while (now() - start < atof(argv[2]) * 1000) {
    double before = now();
    if (sent < trials && before >= next_input) {
      printf("I %.6f %d\n", before, ++sent);
      XTestFakeKeyEvent(d, space, True, CurrentTime);
      XTestFakeKeyEvent(d, space, False, CurrentTime);
      XSync(d, False);
      next_input = before + 250 + rand() % 201;
    }
    double read_start = now();
    if (!XShmGetImage(d, RootWindow(d, screen), im, 0, row, AllPlanes)) return 3;
    double captured = now();
    int left = -1, right = -1;
    for (int x = 0; x < width; x++) {
      unsigned long p = XGetPixel(im, x, 0);
      int r = component(p, im->red_mask), g = component(p, im->green_mask), b = component(p, im->blue_mask);
      if (r < 80 && g > 150 && b > 150) left = x;
      if (left >= 0 && r > 150 && g < 80 && b > 150) { right = x; break; }
    }
    int seq = -1, input = -1;
    if (left >= 0 && right - left >= 144) {
      uint32_t word = 0; int valid = 1;
      for (int i = 0; i < 36; i++) {
        int x = left + (int)((right - left) * (i + .5) / 36);
        int r = component(XGetPixel(im, x, 0), im->red_mask);
        int bit = r > 192 ? 1 : r < 64 ? 0 : -1;
        if (bit < 0 || ((i == 0 || i == 35) && bit != 1) || ((i == 1 || i == 34) && bit != 0)) valid = 0;
        if (i >= 2 && i < 34) word = (word << 1) | (bit > 0);
      }
      int s = word >> 16, in = (word >> 8) & 255;
      if (valid && (word & 255) == (((s >> 8) ^ s ^ in ^ 0xa5) & 255)) { seq = s; input = in; }
    }
    printf("S %.6f %d %d %.6f %.6f\n", captured, seq, input, captured - read_start, now() - before);
    deadline += 4;
    if (deadline < now()) deadline = now();
    until(deadline);
  }
  XShmDetach(d, &shm); XSync(d, False);
  im->data = NULL; XDestroyImage(im); shmdt(shm.shmaddr); XCloseDisplay(d);
  struct rusage usage;
  getrusage(RUSAGE_SELF, &usage);
  printf("Q %.6f %ld\n", (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1000.0 + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1000.0, usage.ru_maxrss);
  return 0;
}
