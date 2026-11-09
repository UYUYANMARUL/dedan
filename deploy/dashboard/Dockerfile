# Demo dashboard: the page is a Next.js static export (web/, shadcn/ui), built in the first stage; stdlib Python
# serves it with the API: the nameserver's views, container logs, and the devnet scripts (add-node.sh and friends,
# which need docker, compose, curl and xxd). The repo is mounted at its host path so compose's bind mounts resolve
# through the docker socket.
FROM node:22-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM python:3.12-alpine
RUN apk add --no-cache docker-cli docker-cli-compose curl xxd
COPY dashboard.py /app/
COPY --from=web /web/out /app/web/out
CMD ["python3", "/app/dashboard.py"]
