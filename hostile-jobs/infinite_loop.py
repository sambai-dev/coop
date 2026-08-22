import time

n = 0
while True:
    n += 1
    if n % 10_000_000 == 0:
        time.sleep(0)
