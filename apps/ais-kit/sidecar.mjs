import { createAisServer, REQUIRED_CAPABILITIES } from "./server.mjs";
import {
  clearTreerInterface,
  registerTreerInterface,
  startRegistrationHeartbeat,
} from "./register.mjs";

export async function runRegisteredAis(options) {
  const capabilities = options.capabilities ?? REQUIRED_CAPABILITIES;
  const ais = createAisServer({ ...options, capabilities });
  const port = await ais.listen(options.port);
  let stopHeartbeat = () => {};
  const shouldRegister = Boolean(process.env.TREER_AGENT_ID)
    && process.env.AIS_AUTO_REGISTER !== "0";
  if (shouldRegister) {
    const register = () => registerTreerInterface({
      port,
      instanceId: options.instanceId,
      capabilities,
      uiPath: options.uiPath,
    });
    await register();
    stopHeartbeat = startRegistrationHeartbeat(register);
  }
  const shutdown = async () => {
    stopHeartbeat();
    if (shouldRegister) await clearTreerInterface();
    await ais.close();
    await options.stop?.();
  };
  process.once("SIGINT", () => { shutdown().finally(() => process.exit(0)); });
  process.once("SIGTERM", () => { shutdown().finally(() => process.exit(0)); });
  return { ais, port, shutdown };
}
