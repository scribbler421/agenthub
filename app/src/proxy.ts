// Routes Node's global fetch through HTTPS_PROXY when one is set (corporate /
// sandboxed environments). Does nothing when the variable is absent.
import { EnvHttpProxyAgent, setGlobalDispatcher } from "undici";
if (process.env.HTTPS_PROXY || process.env.https_proxy) {
  setGlobalDispatcher(new EnvHttpProxyAgent());
}
