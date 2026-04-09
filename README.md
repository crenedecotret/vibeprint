Vibeprint Studio is an ICC aware print layout engine build entirely in RUST.

This was entirely vibe coded. I havent writen a line of code in 30 years.


Please Note: Monitor ICC profile will not load under wayland (for now). It uses specific X11 specific APIs

There are some dependencies required to use Vibeprint Studio


# UBUNTU Core dependencies
sudo apt install \
    libcups2 cups-client libcups2-dev \
    liblcms2-2 liblcms2-dev \
    libx11-6 libx11-dev \
    libxrandr2 libxrandr-dev \
    ghostscript \
    libtiff-tools


# FEDORA Core dependencies
sudo dnf install \
    cups-libs cups-client \
    lcms2 lcms2-devel \
    libX11 libX11-devel \
    libXrandr libXrandr-devel \
    ghostscript \
    libtiff-tools
