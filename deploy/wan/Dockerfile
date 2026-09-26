# Shared image for the WAN-simulated containers that are not nodes: the origin (nginx) and the users (a shell
# with curl and dig); tc for the delays. The nodes run crates/node on the services image.
FROM nginx:alpine
RUN apk add --no-cache iproute2 curl bind-tools
COPY wan.sh /docker-entrypoint.d/05-wan.sh
COPY hosts.txt latency.txt /wan/
RUN chmod +x /docker-entrypoint.d/05-wan.sh
