; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"World\00", align 1
@.str.1 = private unnamed_addr constant [8 x i8] c"Hello, \00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"!\00", align 1
@.str.3 = private unnamed_addr constant [3 x i8] c"x=\00", align 1
@.str.4 = private unnamed_addr constant [7 x i8] c", x*x=\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
@.fmt.concat.ss = private unnamed_addr constant [5 x i8] c"%s%s\00", align 1
@.fmt.concat.sd = private unnamed_addr constant [5 x i8] c"%s%d\00", align 1
declare i32 @snprintf(ptr, i32, ptr, ...)

define i32 @main() {
entry:
  %t0 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t0, i32 256, ptr @.fmt.concat.ss, ptr @.str.1, ptr @.str.0)
  %t2 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t2, i32 256, ptr @.fmt.concat.ss, ptr %t0, ptr @.str.2)
  call i32 @puts(ptr %t2)
  %t5 = add i32 0, 7
  %t6 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t6, i32 256, ptr @.fmt.concat.sd, ptr @.str.3, i32 %t5)
  %t8 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t8, i32 256, ptr @.fmt.concat.ss, ptr %t6, ptr @.str.4)
  %t10 = mul i32 %t5, %t5
  %t11 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t11, i32 256, ptr @.fmt.concat.sd, ptr %t8, i32 %t10)
  call i32 @puts(ptr %t11)
  ret i32 0
}

