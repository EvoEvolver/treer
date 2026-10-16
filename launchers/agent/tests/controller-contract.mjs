// A current Controller reports optional features too. The launcher's
// required_features deliberately excludes private-state so text-only startup
// remains compatible with older Controllers.
export function controllerContractFixture(contract, machine) {
  return {
    git_commit: "a".repeat(40),
    protocol: contract.protocol,
    capabilities: [...contract.capabilities],
    features: [...new Set([...contract.required_features, "agent.private-state.v1"])],
    minimum_launcher_security_epoch: contract.launcher_security_epoch,
    machine,
  };
}
