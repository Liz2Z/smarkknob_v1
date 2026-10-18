#!/usr/bin/env python3
"""Control pulse tests or continuous low-speed rotation with a host keepalive."""

import argparse
import time

import serial


def establish_console(send):
    """Retry only harmless status queries if a reset discards the first one."""
    for attempt in range(3):
        try:
            send(b"?", b"STATUS", timeout=1.0)
            return
        except RuntimeError:
            if attempt == 2:
                raise


def maintain_continuous(send, seconds, monotonic=time.monotonic, sleep=time.sleep):
    deadline = monotonic() + seconds if seconds > 0 else float("inf")
    while monotonic() < deadline:
        send(b"k", b"ALIVE", timeout=0.25)
        sleep(0.1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["status", "forward", "reverse", "continuous", "stop", "diag", "fault", "faultwire", "net"])
    parser.add_argument("--port", default="/dev/cu.usbmodem21201")
    parser.add_argument("--seconds", type=float, default=0, help="Continuous mode duration; 0 runs until Ctrl-C")
    args = parser.parse_args()
    if args.seconds < 0:
        parser.error("--seconds must be non-negative")
    port = serial.Serial(port=None, baudrate=115200, timeout=0.1)
    port.dtr = False
    port.rts = False
    port.port = args.port
    port.open()

    def send(command, expected, timeout=2.0):
        port.write(command + b"\n")
        port.flush()
        deadline = time.monotonic() + timeout
        received = b""
        while expected not in received and time.monotonic() < deadline:
            received += port.read(256)
        for line in received.decode(errors="replace").splitlines():
            if line.startswith(("READY", "STATUS", "STOPPED", "ARMED", "RUN", "REJECTED", "DIAG", "FAULT", "HTTP")):
                print(line, flush=True)
        if moving and b"FAULT LOW" in received:
            raise RuntimeError("DRV8313 fault input is LOW; motor output disabled")
        if expected not in received:
            raise RuntimeError(f"No expected response {expected!r}; received {received!r}")

    moving = args.command in ("forward", "reverse", "continuous")
    try:
        establish_console(send)
        if args.command == "status":
            send(b"?", b"STATUS")
        elif args.command == "stop":
            send(b"s", b"STOPPED")
        elif args.command == "diag":
            send(b"diag", b"EN=0")
        elif args.command == "fault":
            send(b"fault", b"GPIO10")
        elif args.command == "faultwire":
            send(b"faultwire", b"RESTORED=UP EN=0")
        elif args.command == "net":
            send(b"net", b"HTTP")
        elif args.command == "continuous":
            send(b"s", b"STOPPED")
            send(b"arm", b"ARMED")
            send(b"c", b"RUN continuous", timeout=0.25)
            maintain_continuous(send, args.seconds)
            send(b"s", b"STOPPED")
            send(b"?", b"STATUS stopped")
        else:
            send(b"s", b"STOPPED")
            send(b"arm", b"ARMED")
            started = time.monotonic()
            send(b"f" if args.command == "forward" else b"r", b"STOPPED", timeout=4.0)
            elapsed = time.monotonic() - started
            if not 0.8 <= elapsed <= 1.6:
                raise RuntimeError(f"Test stopped early or late ({elapsed:.2f}s)")
            send(b"?", b"STATUS stopped")
    except KeyboardInterrupt:
        print("STOP requested", flush=True)
    finally:
        try:
            if moving:
                port.write(b"s\n")
                port.flush()
        finally:
            port.close()


if __name__ == "__main__":
    main()
