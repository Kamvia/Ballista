// File: ballista/sdks/java/src/main/java/ballista/client/plan/Target.java
package ballista.client.plan;

import ballista.client.internal.Rules;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** The system under test. */
public final class Target {
    private final Map<String, Object> json;

    private Target(Map<String, Object> json) {
        this.json = Collections.unmodifiableMap(new LinkedHashMap<>(json));
    }

    public static Builder builder(String baseUrl) {
        return new Builder(baseUrl);
    }

    public static Target of(String baseUrl) {
        return builder(baseUrl).build();
    }

    Map<String, Object> toJson() {
        return new LinkedHashMap<>(json);
    }

    public static final class Builder {
        private final Map<String, Object> m = new LinkedHashMap<>();
        private final Map<String, String> headers = new LinkedHashMap<>();

        private Builder(String baseUrl) {
            if (!Rules.baseUrl(baseUrl)) {
                throw Rules.fail("target.base_url must be an http(s) URL without credentials");
            }
            m.put("base_url", baseUrl);
        }

        public Builder allowedHosts(String... hosts) { m.put("allowed_hosts", List.of(hosts)); return this; }
        public Builder timeout(String d) { m.put("timeout", Rules.duration(d, "target.timeout")); return this; }
        public Builder connectTimeout(String d) { m.put("connect_timeout", Rules.duration(d, "target.connect_timeout")); return this; }
        public Builder header(String k, String v) { headers.put(k, v); return this; }
        public Builder cookies(boolean on) { m.put("cookies", on); return this; }
        public Builder retries(int n) { m.put("retries", (long) n); return this; }
        public Builder retryBackoff(String d) { m.put("retry_backoff", Rules.duration(d, "target.retry_backoff")); return this; }
        public Builder retryOn(int... codes) {
            m.put("retry_on", java.util.Arrays.stream(codes).mapToObj(c -> (long) c).toList());
            return this;
        }
        public Builder maxBodyBytes(long n) { m.put("max_body_bytes", n); return this; }
        public Builder http2PriorKnowledge(boolean on) { m.put("http2_prior_knowledge", on); return this; }
        /** PEM CA certificate (bundle path) trusted in addition to the platform roots. */
        public Builder caCert(String path) { m.put("ca_cert", path); return this; }
        /** Disables certificate verification of the target (test environments only). */
        public Builder insecureTls(boolean on) { m.put("insecure_tls", on); return this; }

        public Target build() {
            if (!headers.isEmpty()) {
                m.put("headers", Collections.unmodifiableMap(new LinkedHashMap<>(headers)));
            }
            return new Target(m);
        }
    }
}
