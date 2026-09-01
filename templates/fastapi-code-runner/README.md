# FastAPI code runner

This starter targets the upcoming `rookhold==0.8.0` PyPI package. Until that
package is public, install `../../sdks/python` from a Rookhold checkout before
installing the remaining requirements.

```bash
python -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
uvicorn app:app --reload
```

Set `ROOKHOLD_BASE_URL` and `ROOKHOLD_API_KEY` first. The endpoint intentionally
requires gVisor. Add your own user authentication, quotas, and abuse controls.
