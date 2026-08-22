import sys
import time

chunks = []
deadline = time.time() + 20
try:
    while time.time() < deadline:
        chunks.append(bytearray(16 * 1024 * 1024))
except (MemoryError, OSError) as e:
    print(f"allocation refused: {e}")
    sys.exit(2)

print("survived 20s without hitting the cap")
sys.exit(1)
