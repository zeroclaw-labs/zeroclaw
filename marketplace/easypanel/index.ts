import { Output, Services } from "~templates-utils";
import { Input } from "./meta";

export function generate(input: Input): Output {
  const services: Services = [];

  const appEnv = [
    `API_KEY=${input.apiKey}`,
    `PROVIDER=${input.provider}`,
    `ZEROCLAW_ALLOW_PUBLIC_BIND=true`,
    `ZEROCLAW_GATEWAY_PORT=42617`,
  ];

  services.push({
    type: "app",
    data: {
      serviceName: input.appServiceName,
      env: appEnv.join("\n"),
      source: {
        type: "image",
        image: input.appServiceImage,
      },
      domains: [
        {
          host: "$(EASYPANEL_DOMAIN)",
          port: 42617,
        },
      ],
      mounts: [
        {
          type: "volume",
          name: "data",
          mountPath: "/zeroclaw-data",
        },
      ],
    },
  });

  return { services };
}
