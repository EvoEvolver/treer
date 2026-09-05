import { createAisServer, REQUIRED_CAPABILITIES } from "./server.mjs";
import {
  clearTreerInterface,
  registerTreerInterface,
} from "./register.mjs";

export async function runRegisteredAis(options) {
  const capabilities = options.capabilities ?? REQUIRED_CAPABILITIES;
  const ais = createAisServer({ ...options, capabilities });
  const port = await ais.listen(options.port);
  const shouldRegister = Boolean(process.env.TREER_AGENT_ID)
    && process.env.AIS_AUTO_REGISTER !== "0";
  if (shouldRegister) {
    await registerTreerInterface({
      port,
      instanceId: options.instanceId,
      capabilities,
      uiPath: options.uiPath,
    });
  }
  const shutdown = async () => {
    if (shouldRegister) await clearTreerInterface();
    await ais.close();
    await options.stop?.();
  };
  process.once("SIGINT", () => { shutdown().finally(() => process.exit(0)); });
  process.once("SIGTERM", () => { shutdown().finally(() => process.exit(0)); });
  return { ais, port, shutdown };
}
