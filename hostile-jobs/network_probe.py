import socket
import sys

try:
    sock = socket.create_connection(("example.com", 443), timeout=3)
    sock.close()
except OSError as e:
    print(f"network blocked: {e}")
    sys.exit(0)

print("network UP - sandbox leak!")
sys.exit(3)
