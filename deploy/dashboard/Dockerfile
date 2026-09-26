# Demo dashboard: stdlib Python serving deploy/dashboard/index.html, proxying the nameserver's API, tailing
# container logs, and running deploy/scripts/add-node.sh (which needs docker, compose, curl and xxd). The repo
# is mounted at its host path so compose's bind mounts resolve through the docker socket.
FROM python:3.12-alpine
RUN apk add --no-cache docker-cli docker-cli-compose curl xxd
COPY dashboard.py index.html /app/
CMD ["python3", "/app/dashboard.py"]
