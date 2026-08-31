# FastAPI code runner

```bash
python -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
uvicorn app:app --reload
```

Set `ROOKHOLD_BASE_URL` and `ROOKHOLD_API_KEY` first. The endpoint intentionally
requires gVisor. Add your own user authentication, quotas, and abuse controls.
