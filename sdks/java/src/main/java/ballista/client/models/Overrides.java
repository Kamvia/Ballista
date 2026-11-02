// File: ballista/sdks/java/src/main/java/ballista/client/models/Overrides.java
package ballista.client.models;

import ballista.client.internal.Fields;
import ballista.client.internal.Json;
import ballista.client.internal.Rules;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Adjusts a stored plan for one run. Null fields keep the plan's value. */
public record Overrides(Double users, Double spawnRate, String runTime, Long iterations, List<String> tags,
                        List<String> excludeTags, String host, Integer workers) implements Json.JsonValue {
    public static final Overrides NONE = new Overrides(null, null, null, null, null, null, null, null);

    public Overrides {
        tags = tags == null ? null : List.copyOf(tags);
        excludeTags = excludeTags == null ? null : List.copyOf(excludeTags);
    }

    public static Builder builder() {
        return new Builder();
    }

    /** Applies the server rules before anything is sent. */
    public Overrides validate() {
        if (users != null && !(Rules.finite(users) && users > 0)) {
            throw Rules.fail("users must be positive");
        }
        if (spawnRate != null && !(Rules.finite(spawnRate) && spawnRate > 0)) {
            throw Rules.fail("spawn_rate must be positive");
        }
        if (iterations != null && iterations < 1) {
            throw Rules.fail("iterations must be a positive integer");
        }
        if (workers != null && (workers < 0 || workers > 512)) {
            throw Rules.fail("workers must be an integer 0..512");
        }
        if (runTime != null) {
            Rules.duration(runTime, "run_time");
        }
        if (tags != null) {
            Rules.names(tags, 64, "tags");
        }
        if (excludeTags != null) {
            Rules.names(excludeTags, 64, "exclude_tags");
        }
        if (host != null && !Rules.baseUrl(host)) {
            throw Rules.fail("host must be an http(s) base URL without credentials");
        }
        return this;
    }

    @Override
    public Map<String, Object> toJson() {
        Map<String, Object> m = new LinkedHashMap<>();
        if (users != null) m.put("users", users);
        if (spawnRate != null) m.put("spawn_rate", spawnRate);
        if (runTime != null) m.put("run_time", runTime);
        if (iterations != null) m.put("iterations", iterations);
        if (tags != null) m.put("tags", tags);
        if (excludeTags != null) m.put("exclude_tags", excludeTags);
        if (host != null) m.put("host", host);
        if (workers != null) m.put("workers", workers.longValue());
        return m;
    }

    public static Overrides from(Object v) {
        if (v == null) {
            return NONE;
        }
        Fields f = Fields.of(v, "overrides");
        Long w = f.optLng("workers");
        return new Overrides(f.optNum("users"), f.optNum("spawn_rate"), f.optStr("run_time"), f.optLng("iterations"),
                f.has("tags") ? f.strList("tags") : null, f.has("exclude_tags") ? f.strList("exclude_tags") : null,
                f.optStr("host"), w == null ? null : w.intValue());
    }

    public static final class Builder {
        private Double users;
        private Double spawnRate;
        private String runTime;
        private Long iterations;
        private List<String> tags;
        private List<String> excludeTags;
        private String host;
        private Integer workers;

        public Builder users(double v) { users = v; return this; }
        public Builder spawnRate(double v) { spawnRate = v; return this; }
        public Builder runTime(String v) { runTime = v; return this; }
        public Builder iterations(long v) { iterations = v; return this; }
        public Builder tags(String... v) { tags = List.of(v); return this; }
        public Builder excludeTags(String... v) { excludeTags = List.of(v); return this; }
        public Builder host(String v) { host = v; return this; }
        /** Pool workers to use: 0 runs on the server only; unset uses every available worker. */
        public Builder workers(int v) { workers = v; return this; }

        public Overrides build() {
            return new Overrides(users, spawnRate, runTime, iterations, tags, excludeTags, host, workers).validate();
        }
    }
}
