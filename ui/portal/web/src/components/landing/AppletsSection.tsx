import { useFavApplets } from "@left-curve/store";

import { APPLETS } from "~/constants";

import { IconAddCross, twMerge, useApp } from "@left-curve/applets-kit";
import { Link } from "@tanstack/react-router";
import { Image } from "~/components/foundation/Image";

export function AppletsSection() {
  const { favApplets } = useFavApplets();
  const { setSearchBarVisibility } = useApp();

  return (
    <div className="grid grid-cols-[repeat(auto-fill,_minmax(80px,_1fr))] md:flex md:flex-wrap gap-4 md:gap-4 w-full md:min-h-fit md:justify-start">
      {favApplets.map((appletId) => {
        const applet = APPLETS[appletId];
        if (!applet) return null;
        return (
          <div
            className="md:w-[5.5rem] h-auto flex items-start justify-center md:[&:nth-child(6n)]:flex-1 md:[&:nth-child(6n)]:justify-end md:[&:nth-child(6n+1)]:justify-start"
            key={`applets.section.${applet.title}}`}
          >
            <div className="flex flex-col items-center gap-2">
              {applet.isDisabled ? (
                <button
                  type="button"
                  aria-label={applet.title}
                  disabled
                  className="h-16 w-16 cursor-not-allowed rounded-xl bg-surface-disabled-gray p-[10px] shadow-account-card md:h-20 md:w-20"
                >
                  <Image src={applet.img} alt="" className="h-full w-full grayscale opacity-50" />
                </button>
              ) : (
                <Link
                  to={applet.path}
                  className="h-16 w-16 md:h-20 md:w-20 shadow-account-card bg-surface-primary-red hover:bg-surface-secondary-red transition-all rounded-xl p-[10px]"
                >
                  <Image src={applet.img} alt={applet.title} className="w-full h-full" />
                </Link>
              )}
              <div className="md:w-[5rem] h-[36.41px] relative">
                <p
                  className={twMerge(
                    "text-center text-sm font-bold",
                    applet.isDisabled && "text-fg-disabled",
                  )}
                >
                  {applet.title}
                </p>
              </div>
            </div>
          </div>
        );
      })}

      <div className="md:w-[5.5rem] h-auto flex items-start justify-center md:[&:nth-child(6n)]:flex-1 md:[&:nth-child(6n)]:justify-end md:[&:nth-child(6n+1)]:justify-start">
        <button
          type="button"
          onClick={() => setSearchBarVisibility(true)}
          className="h-16 w-16 md:h-20 md:w-20 shadow-account-card border-[1.43px] border-outline-tertiary-rice text-outline-tertiary-rice bg-surface-primary-rice hover:bg-surface-secondary-rice transition-all rounded-xl p-[10px] flex items-center justify-center cursor-pointer"
        >
          <IconAddCross />
        </button>
        <p className="min-h-6" />
      </div>
    </div>
  );
}
