# Shared image for every WAN-simulated container: nginx (mock node or origin), tc for delays, curl and dig for probes.
FROM nginx:alpine
RUN apk add --no-cache iproute2 curl bind-tools
COPY wan.sh /docker-entrypoint.d/05-wan.sh
COPY hosts.txt latency.txt /wan/
COPY dedan.js /etc/nginx/dedan.js
COPY 20-blob.sh /docker-entrypoint.d/20-blob.sh
RUN chmod +x /docker-entrypoint.d/05-wan.sh /docker-entrypoint.d/20-blob.sh \
    && sed -i '1i load_module modules/ngx_http_js_module.so;' /etc/nginx/nginx.conf
