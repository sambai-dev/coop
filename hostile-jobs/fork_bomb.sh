spawned=0
for i in $(seq 1 200); do
    if (sleep 5) 2>/dev/null & then
        spawned=$((spawned + 1))
    fi
done
echo "spawned=$spawned"
if [ "$spawned" -gt 100 ]; then
    echo "FORK BOMB NOT CONTAINED"
    exit 1
fi
exit 0
