# The steer engine image. Build from the repo root:
#   docker build -t playbox-steer:0.1 -f steer/Dockerfile steer
# Weights are mounted read-only at /weights by the host; there is no network and no model inside the image.
FROM python:3.12-slim-bookworm
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1
WORKDIR /app
RUN pip install --no-cache-dir torch==2.5.1 --index-url https://download.pytorch.org/whl/cpu \
 && pip install --no-cache-dir "transformers==4.46.3" "numpy<2.2" "blake3>=0.4" "safetensors>=0.4"
COPY playbox_steer /app/playbox_steer
ENV PYTHONPATH=/app
# The host chooses the command; this keeps `docker run IMAGE` harmless.
CMD ["python", "-m", "playbox_steer", "--help"]
