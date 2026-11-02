// File: ballista/sdks/java/src/main/java/ballista/client/internal/Tls.java
package ballista.client.internal;

import ballista.client.exceptions.ConfigurationException;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.cert.CertificateException;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;

import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.TrustManagerFactory;
import javax.net.ssl.X509TrustManager;

/** TLS context trusting the platform roots plus an optional PEM CA bundle. */
public final class Tls {
    private Tls() {
    }

    private static X509TrustManager trustManager(KeyStore ks) throws GeneralSecurityException {
        TrustManagerFactory tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
        tmf.init(ks);
        for (TrustManager tm : tmf.getTrustManagers()) {
            if (tm instanceof X509TrustManager x) {
                return x;
            }
        }
        throw new GeneralSecurityException("no X509 trust manager");
    }

    public static SSLContext withExtraCa(Path pemFile) {
        try (InputStream in = Files.newInputStream(pemFile)) {
            Collection<? extends java.security.cert.Certificate> certs = CertificateFactory.getInstance("X.509").generateCertificates(in);
            if (certs.isEmpty()) {
                throw new ConfigurationException("ca cert contains no certificates");
            }
            KeyStore ks = KeyStore.getInstance(KeyStore.getDefaultType());
            ks.load(null, null);
            int i = 0;
            for (java.security.cert.Certificate c : certs) {
                ks.setCertificateEntry("extra-" + i++, c);
            }
            X509TrustManager platform = trustManager(null);
            X509TrustManager extra = trustManager(ks);
            SSLContext ctx = SSLContext.getInstance("TLS");
            ctx.init(null, new TrustManager[] {new Composite(List.of(platform, extra))}, null);
            return ctx;
        } catch (IOException | GeneralSecurityException e) {
            throw new ConfigurationException("ca cert: " + e.getMessage());
        }
    }

    private record Composite(List<X509TrustManager> managers) implements X509TrustManager {
        @Override
        public void checkClientTrusted(X509Certificate[] chain, String authType) throws CertificateException {
            throw new CertificateException("client certificates are not accepted");
        }

        @Override
        public void checkServerTrusted(X509Certificate[] chain, String authType) throws CertificateException {
            CertificateException last = null;
            for (X509TrustManager m : managers) {
                try {
                    m.checkServerTrusted(chain, authType);
                    return;
                } catch (CertificateException e) {
                    last = e;
                }
            }
            throw last == null ? new CertificateException("untrusted server certificate") : last;
        }

        @Override
        public X509Certificate[] getAcceptedIssuers() {
            List<X509Certificate> out = new ArrayList<>();
            for (X509TrustManager m : managers) {
                out.addAll(List.of(m.getAcceptedIssuers()));
            }
            return out.toArray(X509Certificate[]::new);
        }
    }
}
