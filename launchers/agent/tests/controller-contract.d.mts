export function controllerContractFixture<Machine>(contract: {
  protocol: string;
  capabilities: string[];
  required_features: string[];
  launcher_security_epoch: number;
}, machine: Machine): {
  git_commit: string;
  protocol: string;
  capabilities: string[];
  features: string[];
  minimum_launcher_security_epoch: number;
  machine: Machine;
};
