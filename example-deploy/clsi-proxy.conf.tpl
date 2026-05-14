# Bearer-adding pass-through to clsi-rs on Cloudflare.
# nginx:alpine runs envsubst on /etc/nginx/templates/*.template at startup,
# so ${CLSI_SHARED_AUTH} and ${CLSI_CF_HOST} get expanded from the container's
# env before nginx loads the config.
server {
    listen 3013;
    server_name _;
    resolver 1.1.1.1 8.8.8.8 ipv6=off valid=300s;
    resolver_timeout 5s;

    # Compile responses can be a few MB (log + synctex + fls etc); be generous.
    client_max_body_size 64m;
    proxy_buffering off;
    proxy_request_buffering off;

    location / {
        proxy_pass https://${CLSI_CF_HOST}$request_uri;
        proxy_set_header Authorization "Bearer ${CLSI_SHARED_AUTH}";
        proxy_set_header Host ${CLSI_CF_HOST};
        proxy_ssl_server_name on;
        proxy_http_version 1.1;
        proxy_read_timeout 600s;   # latex compile can take a while
        proxy_send_timeout 600s;
        proxy_connect_timeout 30s;
    }
}
