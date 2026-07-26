import type { Metadata } from "next";
import { connection } from "next/server";
import type { ReactNode } from "react";
import { Providers } from "../components/providers";
import "./styles.css";

export const metadata: Metadata = {
  title: "nanoctl",
  description: "Fast, secure remote access to your computers.",
};

export default async function RootLayout({
  children,
}: Readonly<{
  children: ReactNode;
}>) {
  await connection();
  return (
    <html lang="en">
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
