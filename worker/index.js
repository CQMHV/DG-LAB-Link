const SECURITY_HEADERS = {
    "Content-Security-Policy": [
        "default-src 'self'",
        "script-src 'self'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self' data: blob:",
        "font-src 'self'",
        "connect-src 'self'",
        "object-src 'none'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
        "form-action 'none'",
    ].join("; "),
    "Cross-Origin-Opener-Policy": "same-origin",
    "Permissions-Policy": "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    "Referrer-Policy": "no-referrer",
    "X-Content-Type-Options": "nosniff",
};

const withSecurityHeaders = (response) => {
    const headers = new Headers(response.headers);
    for (const [name, value] of Object.entries(SECURITY_HEADERS)) {
        headers.set(name, value);
    }

    return new Response(response.body, {
        headers,
        status: response.status,
        statusText: response.statusText,
    });
};

export default {
    async fetch(request, env) {
        const response = await env.ASSETS.fetch(request);
        const acceptsHtml = request.headers.get("accept")?.includes("text/html");

        if (response.status !== 404 || !acceptsHtml || !["GET", "HEAD"].includes(request.method)) {
            return withSecurityHeaders(response);
        }

        const indexUrl = new URL(request.url);
        indexUrl.pathname = "/index.html";
        indexUrl.search = "";
        return withSecurityHeaders(await env.ASSETS.fetch(new Request(indexUrl, request)));
    },
};
