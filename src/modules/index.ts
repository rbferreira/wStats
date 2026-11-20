import type { ModuleId } from "../types";
import { batteryModule } from "./battery";
import type { Module } from "./common";
import { cpuModule } from "./cpu";
import { devicesModule } from "./devices";
import { diskModule } from "./disk";
import { gpuModule } from "./gpu";
import { memModule } from "./mem";
import { netModule } from "./net";
import { npuModule } from "./npu";
import { fansModule } from "./fans";
import { powerModule } from "./power";
import { tempModule } from "./temp";

export const MODULES: Record<ModuleId, () => Module> = {
  cpu: cpuModule,
  gpu: gpuModule,
  mem: memModule,
  net: netModule,
  disk: diskModule,
  battery: batteryModule,
  temp: tempModule,
  fans: fansModule,
  power: powerModule,
  npu: npuModule,
  devices: devicesModule,
  // The ping card lives in the network menu.
  latency: netModule,
};
